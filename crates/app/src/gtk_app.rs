//! GTK4 应用外壳。
//!
//! 本模块只负责 UI 组合和流程编排。终端解析、协议 I/O、配置持久化和渲染基础
//! 能力都放在独立包中，因此可以在没有显示服务器的环境中测试。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use gtk4::glib::{self, ControlFlow};
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, CssProvider, DrawingArea,
    DropTarget, Entry, EventControllerMotion, EventControllerScroll, EventControllerScrollFlags,
    Expander, FileChooserAction, FileChooserNative, GestureClick, Grid, HeaderBar, Image, Label,
    ListBox, ListBoxRow, Notebook, Orientation, Paned, PolicyType, Popover, PositionType,
    ProgressBar, ResponseType, ScrolledWindow, SelectionMode, Stack, StackSwitcher,
    StackTransitionType, Widget, Window,
};
use gtk4::{gdk, gio, pango};
use serde::{Deserialize, Serialize};
use shell_core::{
    AuthConfig, ConnectionProfile, CredentialsRef, ProtocolEvent, ProtocolKind, TerminalSize,
};
use shell_platform::{BuiltinToolchain, PtyLaunchOptions, ToolchainPathPriority};
use shell_protocol::{
    FtpConfig, FtpConnection, HostStats, HostStatsConfig, LocalShellConnection, SerialConfig,
    SerialConnection, SftpConfig, SftpSession, SshConfig, SshConnection, TelnetConfig,
    TelnetConnection, fetch_ssh_host_stats,
};
use shell_renderer::gtk::{GtkTerminalView, TerminalAppearance, adjusted_font_description_size};
use shell_storage::{
    AppLanguage, AppSettings, BuiltinToolsPathPriority, BuiltinToolsSettings,
    OpensshCompatibilitySettings, ProfileStore, ProfilesDocument, RendererBackend, delete_secret,
    load_secret, store_secret,
};
use shell_terminal::TerminalBuffer;

mod formatting;
mod session_tabs;
mod sftp_ui;

use formatting::*;
use session_tabs::*;
use sftp_ui::*;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
// 默认历史行数保持保守：即使命令输出数百万字符，终端输出内存也必须保持有界。
const DEFAULT_SCROLLBACK_LINES: usize = 2_000;
const LOCAL_TERMINALS_CACHE_VERSION: u32 = 2;
// 会话标签允许缩小，但不能低于可读宽度。标签栏溢出后优先使用横向滚动，而不是
// 隐藏 IP/协议信息或让关闭按钮发生意外位移。
const SIDEBAR_STACK_TRANSITION_MS: u32 = 180;
const PAGE_SWITCH_TRANSITION_MS: u64 = 160;
const WORKSPACE_STACK_TRANSITION_MS: u32 = 180;
const WORKSPACE_PAGE_EMPTY: &str = "empty";
const WORKSPACE_PAGE_SESSIONS: &str = "sessions";
const TERMINAL_FONT_PRESETS: [(&str, &str); 5] = [
    (
        "Cascadia Mono + Emoji/CJK",
        "Cascadia Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13",
    ),
    (
        "Consolas + Emoji/CJK",
        "Consolas, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13",
    ),
    (
        "JetBrains Mono + Emoji/CJK",
        "JetBrains Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13",
    ),
    (
        "Fira Code + Emoji/CJK",
        "Fira Code, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13",
    ),
    (
        "Sarasa Mono SC + Emoji",
        "Sarasa Mono SC, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13",
    ),
];

fn tr(language: &AppLanguage, zh_cn: &'static str, en_us: &'static str) -> &'static str {
    match language {
        AppLanguage::ZhCn => zh_cn,
        AppLanguage::EnUs => en_us,
    }
}

fn host_idle_text(language: &AppLanguage) -> &'static str {
    tr(language, "本地", "Local")
}

pub fn run() -> anyhow::Result<()> {
    if std::env::var_os("GSK_RENDERER").is_none() {
        // 默认优先使用 GTK 的 CPU 渲染器；它与终端的 Cairo 优先设计一致，
        // 也能避免不必要的图形后端开销。
        unsafe {
            std::env::set_var("GSK_RENDERER", "cairo");
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let app = Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(build_ui);
    app.run();
    Ok(())
}

fn build_ui(app: &Application) {
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
    }
    install_app_css();

    let connections = Rc::new(RefCell::new(SessionHandles::default()));
    let notebook = Notebook::builder().hexpand(true).vexpand(true).build();
    notebook.set_show_tabs(false);
    notebook.set_show_border(false);
    let profile_store = ProfileStore::new(profiles_path());
    let app_settings_value = profile_store.load_settings().unwrap_or_default();
    let initial_language = app_settings_value.language.clone();

    let status = Label::new(Some(tr(&initial_language, "就绪", "Ready")));
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_ellipsize(pango::EllipsizeMode::End);
    status.add_css_class("status-label");
    let host_info = Label::new(Some(host_idle_text(&initial_language)));
    host_info.set_xalign(0.0);
    host_info.set_ellipsize(pango::EllipsizeMode::End);
    host_info.add_css_class("host-info");

    let terminal_appearance = TerminalAppearance::new(app_settings_value.terminal_font.clone());
    terminal_appearance.set_semantic_highlighting(app_settings_value.semantic_highlighting);
    let local_terminals_value = load_or_discover_local_terminals(&app_settings_value);
    let app_settings = Rc::new(RefCell::new(app_settings_value));
    let mut loaded_profiles = profile_store.load().unwrap_or_default();
    if remove_local_shell_profiles(&mut loaded_profiles) {
        let _ = profile_store.save(&loaded_profiles);
    }
    let profiles_doc = Rc::new(RefCell::new(loaded_profiles));
    let local_terminals = Rc::new(local_terminals_value);
    let profiles_list = ListBox::new();
    profiles_list.set_selection_mode(SelectionMode::Single);
    profiles_list.set_activate_on_single_click(false);
    profiles_list.add_css_class("profiles-list");
    let session_tab_strip = build_session_tab_strip(&initial_language);

    let header = HeaderBar::builder()
        .title_widget(&Label::new(Some("R-shell")))
        .build();
    header.add_css_class("app-header");
    let new_session_btn = Button::with_label(tr(&initial_language, "新建", "New"));
    let settings_btn = Button::with_label(tr(&initial_language, "设置", "Settings"));
    new_session_btn.add_css_class("toolbar-button");
    settings_btn.add_css_class("toolbar-button");
    header.pack_start(&new_session_btn);
    header.pack_end(&settings_btn);

    let sidebar = GtkBox::new(Orientation::Horizontal, 10);
    sidebar.set_hexpand(false);
    sidebar.set_margin_top(10);
    sidebar.set_margin_bottom(10);
    sidebar.set_margin_start(10);
    sidebar.set_margin_end(10);
    sidebar.set_size_request(300, -1);
    sidebar.add_css_class("session-sidebar");

    let nav_rail = GtkBox::new(Orientation::Vertical, 8);
    nav_rail.add_css_class("sidebar-nav");
    let sessions_nav_btn = build_sidebar_nav_button(
        "utilities-terminal-symbolic",
        tr(&initial_language, "已保存会话", "Sessions"),
    );
    let sftp_nav_btn = build_sidebar_nav_button(
        "folder-symbolic",
        tr(&initial_language, "SFTP 浏览器", "SFTP browser"),
    );
    nav_rail.append(&sessions_nav_btn);
    nav_rail.append(&sftp_nav_btn);

    let sessions_panel = GtkBox::new(Orientation::Vertical, 8);
    sessions_panel.set_size_request(0, -1);
    sessions_panel.add_css_class("sidebar-panel");
    let sidebar_title = Label::new(Some(tr(&initial_language, "已保存会话", "Saved Sessions")));
    sidebar_title.set_xalign(0.0);
    sidebar_title.add_css_class("sidebar-panel-title");
    let list_scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&profiles_list)
        .build();
    list_scroll.add_css_class("sidebar-scroll");
    sessions_panel.append(&sidebar_title);
    sessions_panel.append(&list_scroll);

    let sftp_sidebar = build_sftp_sidebar(true, &initial_language);
    let sidebar_stack = Stack::new();
    sidebar_stack.set_hexpand(true);
    sidebar_stack.set_size_request(0, -1);
    sidebar_stack.set_vexpand(true);
    sidebar_stack.set_hhomogeneous(false);
    sidebar_stack.set_vhomogeneous(false);
    sidebar_stack.set_transition_type(StackTransitionType::Crossfade);
    sidebar_stack.set_transition_duration(SIDEBAR_STACK_TRANSITION_MS);
    sidebar_stack.add_titled(&sessions_panel, Some("sessions"), "Sessions");
    sidebar_stack.add_titled(&sftp_sidebar.container, Some("sftp"), "SFTP");
    sidebar_stack.set_visible_child_name("sessions");

    sidebar.append(&nav_rail);
    sidebar.append(&sidebar_stack);

    let (empty_state, empty_open_local_btn, empty_new_session_btn) =
        build_empty_workspace(&initial_language);
    let workspace_stack = Stack::new();
    workspace_stack.set_hexpand(true);
    workspace_stack.set_vexpand(true);
    workspace_stack.set_hhomogeneous(false);
    workspace_stack.set_vhomogeneous(false);
    workspace_stack.set_transition_type(StackTransitionType::Crossfade);
    workspace_stack.set_transition_duration(WORKSPACE_STACK_TRANSITION_MS);
    workspace_stack.add_named(&empty_state, Some(WORKSPACE_PAGE_EMPTY));
    workspace_stack.add_named(&notebook, Some(WORKSPACE_PAGE_SESSIONS));
    workspace_stack.set_visible_child_name(WORKSPACE_PAGE_EMPTY);

    let status_bar = GtkBox::new(Orientation::Horizontal, 12);
    status_bar.add_css_class("app-status-bar");
    status_bar.append(&host_info);
    status_bar.append(&status);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.add_css_class("content-panel");
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_size_request(0, -1);
    content.append(&session_tab_strip.container);
    content.append(&workspace_stack);
    content.append(&status_bar);

    let root = Paned::new(Orientation::Horizontal);
    root.add_css_class("app-root");
    root.set_start_child(Some(&sidebar));
    root.set_end_child(Some(&content));
    root.set_position(300);
    root.set_wide_handle(false);
    root.set_resize_start_child(false);
    root.set_shrink_start_child(true);
    root.set_resize_end_child(true);
    root.set_shrink_end_child(true);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("R-shell")
        .default_width(1360)
        .default_height(800)
        .child(&root)
        .build();
    window.add_css_class("app-window");
    window.set_titlebar(Some(&header));

    let state = AppState {
        window: window.clone(),
        notebook: notebook.clone(),
        workspace_stack: workspace_stack.clone(),
        status: status.clone(),
        host_info: host_info.clone(),
        profiles_list: profiles_list.clone(),
        profiles_doc: Rc::clone(&profiles_doc),
        profile_store,
        app_settings,
        terminal_appearance,
        terminal_widgets: Rc::new(RefCell::new(Vec::new())),
        page_tabs: Rc::new(RefCell::new(Vec::new())),
        page_contexts: Rc::new(RefCell::new(Vec::new())),
        page_shutdowns: Rc::new(RefCell::new(Vec::new())),
        connections,
        host_stats_poller: Rc::new(RefCell::new(None)),
        window_size_cap: Rc::new(RefCell::new(None)),
        active_popover: Rc::new(RefCell::new(None)),
        local_terminals,
        chrome: AppChrome {
            new_session_button: new_session_btn.clone(),
            settings_button: settings_btn.clone(),
            sidebar_title: sidebar_title.clone(),
        },
        session_tab_strip: session_tab_strip.clone(),
        sidebar_nav: SidebarNav {
            stack: sidebar_stack.clone(),
            sessions_button: sessions_nav_btn.clone(),
            sftp_button: sftp_nav_btn.clone(),
        },
        sftp_sidebar: sftp_sidebar.clone(),
    };
    refresh_profile_list_for_state(&state);

    let state_for_new = state.clone();
    new_session_btn.connect_clicked(move |_| {
        show_new_session_window(&state_for_new);
    });

    let state_for_empty_new = state.clone();
    empty_new_session_btn.connect_clicked(move |_| {
        show_new_session_window(&state_for_empty_new);
    });

    let state_for_local_add = state.clone();
    session_tab_strip.add_button.connect_clicked(move |_| {
        open_preferred_local_terminal(&state_for_local_add);
    });

    let state_for_empty_local = state.clone();
    empty_open_local_btn.connect_clicked(move |_| {
        open_preferred_local_terminal(&state_for_empty_local);
    });

    let state_for_local_dropdown = state.clone();
    let dropdown_button = session_tab_strip.dropdown_button.clone();
    dropdown_button.connect_clicked(move |button| {
        show_local_terminal_popover(&state_for_local_dropdown, button);
    });

    let state_for_settings = state.clone();
    settings_btn.connect_clicked(move |_| {
        open_settings_tab(&state_for_settings);
    });

    let state_for_sessions_nav = state.clone();
    sessions_nav_btn.connect_clicked(move |_| {
        show_sidebar_section(&state_for_sessions_nav, "sessions");
    });

    let state_for_sftp_nav = state.clone();
    sftp_nav_btn.connect_clicked(move |_| {
        show_sidebar_section(&state_for_sftp_nav, "sftp");
        activate_sftp_sidebar_for_current_session(&state_for_sftp_nav);
    });

    let state_for_switch_page = state.clone();
    notebook.connect_switch_page(move |_, page, _| {
        animate_notebook_switch(&state_for_switch_page.notebook);
        let context = page_context_for_widget(&state_for_switch_page, page);
        sync_active_page_state(&state_for_switch_page, context);
        sync_session_tab_strip(&state_for_switch_page, Some(page));
        schedule_widget_focus(page);
    });

    let state_for_page_removed = state.clone();
    notebook.connect_page_removed(move |_, page, _| {
        invoke_page_shutdown(&state_for_page_removed, page);
        prune_page_tabs(&state_for_page_removed);
        prune_page_contexts(&state_for_page_removed);
        schedule_session_handle_prune(&state_for_page_removed);
        schedule_post_close_memory_trim();
        sync_session_tab_strip(&state_for_page_removed, None);
        sync_workspace_state(&state_for_page_removed);
        let context = current_page_context(&state_for_page_removed);
        sync_active_page_state(&state_for_page_removed, context);
    });

    let state_for_activate = state.clone();
    profiles_list.connect_row_activated(move |_, row| {
        let index = row.index() as usize;
        if let Some(profile) = state_for_activate
            .profiles_doc
            .borrow()
            .profiles
            .get(index)
            .cloned()
        {
            connect_saved_profile(&state_for_activate, profile);
        }
    });
    connect_profiles_context_menu(&state);
    connect_sftp_sidebar_actions(&state);
    show_sidebar_section(&state, "sessions");
    sync_session_tab_strip(&state, None);
    sync_workspace_state(&state);

    window.present();
    schedule_session_tab_width_cap_capture(&state);
    schedule_debug_local_terminal_autostart(&state);
    schedule_debug_tab_autoclose(&state);
    schedule_startup_memory_trim();
}

#[cfg(windows)]
fn schedule_startup_memory_trim() {
    for seconds in [2_u64, 6, 20] {
        glib::timeout_add_local(Duration::from_secs(seconds), || {
            trim_process_working_set();
            ControlFlow::Break
        });
    }
}

fn schedule_session_tab_width_cap_capture(state: &AppState) {
    let state_for_capture = state.clone();
    glib::idle_add_local_once(move || {
        capture_session_tab_width_cap(&state_for_capture);
        capture_window_size_cap(&state_for_capture);
    });
}

fn capture_session_tab_width_cap(state: &AppState) {
    let width = state.session_tab_strip.container.allocated_width();
    if width > 0 {
        *state.session_tab_strip.width_cap.borrow_mut() = Some(width);
    }
}

fn capture_window_size_cap(state: &AppState) {
    let width = state.window.allocated_width();
    let height = state.window.allocated_height();
    if width > 0 && height > 0 {
        *state.window_size_cap.borrow_mut() = Some((width, height));
    }
}

#[cfg(not(windows))]
fn schedule_startup_memory_trim() {}

#[cfg(windows)]
fn trim_process_working_set() {
    use windows_sys::Win32::System::ProcessStatus::EmptyWorkingSet;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

fn install_app_css() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let provider = CssProvider::new();
    provider.load_from_data(
        r#"
        * {
            color: #d7dce2;
        }
        button, combobox, combobox button, listbox row, entry, checkbutton {
            outline: none;
            outline-offset: 0;
        }
        button, entry, combobox button, listbox row, .session-tab-item, .sidebar-nav-button,
        .sftp-toolbar-button, .profile-actions-button, popover > contents,
        .settings-page, .settings-card, .new-session-form, .dialog-content,
        .protocol-page, notebook, notebook > stack, stack, .empty-workspace, .app-status-bar {
            transition-property: background-color, border-color, color, opacity, box-shadow;
            transition-duration: 180ms;
            transition-timing-function: ease-out;
        }
        notebook.page-switching > stack {
            opacity: 0.94;
        }
        window, window.background, .app-window, .app-root, .content-panel, .session-sidebar,
        .sidebar-scroll, .sidebar-scroll viewport, .sidebar-scroll overshoot, .sidebar-scroll undershoot,
        listbox, listbox.view, .profiles-list, .profiles-list viewport, .profiles-list.view,
        scrolledwindow, scrolledwindow viewport, viewport, .app-status-bar,
        notebook, notebook > stack, textview, textview text {
            background: #0c1014;
            background-color: #0c1014;
            color: #d7dce2;
        }
        headerbar, .app-header, .app-header windowhandle {
            background: #0d1218;
            background-color: #0d1218;
            color: #d7dce2;
            border-bottom: 1px solid #242a30;
        }
        popover > contents, .app-menu-popover > contents {
            background-color: #151b22;
            color: #d7dce2;
            border: 1px solid #33404d;
            border-radius: 12px;
            padding: 6px;
            box-shadow: 0 14px 34px rgba(0, 0, 0, 0.38);
        }
        listbox row, listbox row box, listbox row label, .session-sidebar label, .content-panel label {
            color: #d7dce2;
            background-color: transparent;
        }
        button, menubutton > button, combobox button, combobox box button {
            background: #1a222b;
            background-color: #1a222b;
            background-image: none;
            color: #d7dce2;
            border: 1px solid #303d49;
            border-radius: 8px;
            box-shadow: none;
            padding: 5px 10px;
        }
        button:hover, menubutton > button:hover, combobox button:hover {
            background: #24303b;
            background-color: #24303b;
            border-color: #415267;
        }
        button:active, button:checked, menubutton > button:active, combobox button:active {
            background: #2d4052;
            background-color: #2d4052;
            border-color: #4d6680;
        }
        .profile-actions-button {
            min-width: 150px;
            padding: 8px 12px;
            border-color: transparent;
            background: transparent;
            background-color: transparent;
            border-radius: 8px;
        }
        .profile-actions-button:hover {
            background: #24303b;
            background-color: #24303b;
            border-color: #33465a;
        }
        .context-menu-list {
            padding: 0;
            background: transparent;
            background-color: transparent;
        }
        .context-menu-button,
        .context-menu-button:hover,
        .context-menu-button:active,
        .context-menu-button:checked,
        .context-menu-button:disabled {
            margin: 0;
            border-radius: 8px;
            box-shadow: none;
            background-image: none;
        }
        .context-menu-button:disabled {
            opacity: 0.42;
        }
        .profile-sftp-button {
            min-width: 34px;
            min-height: 34px;
            padding: 4px;
            border-radius: 6px;
        }
        .profile-sftp-button:disabled {
            opacity: 0.35;
        }
        .sftp-toolbar-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: 6px;
        }
        .sftp-header-row {
            padding: 4px 0;
            border-bottom: 1px solid #242a30;
        }
        .sftp-header-label {
            color: #f0f3f6;
            font-weight: 700;
        }
        .sftp-name-column {
            font-weight: 600;
        }
        .sftp-metadata-column {
            color: #c8cfd8;
        }
        button label {
            background: transparent;
            background-color: transparent;
            border: none;
            box-shadow: none;
        }
        button.flat, button.flat:hover, button.flat:active, button.flat:checked,
        .tab-close-button, .tab-close-button:hover, .tab-close-button:active, .tab-close-button:checked {
            background: transparent;
            background-color: transparent;
            background-image: none;
            border: none;
            border-radius: 0;
            box-shadow: none;
            padding: 0 4px;
        }
        .tab-close-button:hover {
            color: #f5f7fa;
            background-color: #26303a;
            border-radius: 4px;
        }
        .toolbar-button {
            min-width: 68px;
        }
        headerbar windowcontrols,
        headerbar windowcontrols box,
        headerbar windowcontrols image {
            background: transparent;
            background-color: transparent;
            border: none;
            box-shadow: none;
        }
        headerbar windowcontrols button,
        headerbar windowcontrols button:hover,
        headerbar windowcontrols button:active,
        headerbar windowcontrols button:checked {
            background: transparent;
            background-color: transparent;
            background-image: none;
            border: none;
            border-radius: 0;
            box-shadow: none;
            outline: none;
            padding: 6px 10px;
        }
        headerbar windowcontrols button:hover {
            background-color: #26303a;
        }
        headerbar windowcontrols button.close:hover {
            background-color: #b13a3a;
            color: #f5f7fa;
        }
        entry, entry:focus, textview, textview text {
            background: #11161b;
            background-color: #11161b;
            color: #d7dce2;
            border: 1px solid #303842;
            border-radius: 6px;
        }
        entry text,
        entry image,
        textview text {
            background: transparent;
            background-color: transparent;
            border: none;
            box-shadow: none;
        }
        combobox,
        combobox:hover,
        combobox:focus,
        combobox box {
            background: transparent;
            background-color: transparent;
            border: none;
            border-image: none;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            padding: 0;
            margin: 0;
            transform: none;
        }
        combobox button,
        combobox button:focus,
        combobox button:hover,
        combobox button:active,
        combobox button:checked {
            background: #11161b;
            background-color: #11161b;
            background-image: none;
            color: #d7dce2;
            border: 1px solid #303842;
            border-image: none;
            border-radius: 6px;
            box-shadow: none;
            min-height: 36px;
            padding: 0 10px;
            margin: 0;
            outline-style: none;
            outline-width: 0;
            outline-offset: 0;
            text-shadow: none;
            -gtk-icon-shadow: none;
            transition: none;
            transform: none;
        }
        combobox button:hover,
        combobox button:focus,
        combobox button:checked {
            border-color: #303842;
        }
        combobox button box,
        combobox button cellview,
        combobox button label,
        combobox button image,
        combobox arrow {
            background: transparent;
            background-color: transparent;
            border: none;
            border-image: none;
            box-shadow: none;
            min-height: 0;
            margin: 0;
            padding: 0;
            outline-style: none;
            outline-width: 0;
            text-shadow: none;
            -gtk-icon-shadow: none;
            transform: none;
            transition: none;
        }
        combobox button box {
            border-spacing: 0;
        }
        combobox arrow {
            min-width: 18px;
        }
        .stable-combo,
        .stable-combo:hover,
        .stable-combo:focus,
        .stable-combo > box,
        .stable-combo > box:hover {
            min-height: 36px;
            padding: 0;
            margin: 0;
            border: 0 solid transparent;
            border-image: none;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            transform: none;
            transition: none;
        }
        .stable-combo button,
        .stable-combo button:hover,
        .stable-combo button:focus,
        .stable-combo button:active,
        .stable-combo button:checked {
            min-height: 36px;
            padding: 0 10px;
            margin: 0;
            border-width: 1px;
            border-style: solid;
            border-color: #303842;
            border-image: none;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            outline-offset: 0;
            transform: none;
            transition: none;
        }
        .stable-combo cellview,
        .stable-combo cellview:hover,
        .stable-combo label,
        .stable-combo label:hover,
        .stable-combo arrow,
        .stable-combo arrow:hover {
            margin: 0;
            padding: 0;
            border: 0 solid transparent;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            text-shadow: none;
            -gtk-icon-shadow: none;
            transform: none;
            transition: none;
        }
        .stable-dropdown,
        .stable-dropdown:hover,
        .stable-dropdown:focus,
        .stable-dropdown:active,
        .stable-dropdown:checked {
            min-height: 36px;
            padding: 0 10px;
            margin: 0;
            background: #11161b;
            background-color: #11161b;
            background-image: none;
            border: 1px solid #303842;
            border-radius: 6px;
            border-image: none;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            outline-offset: 0;
            transform: none;
            transition: none;
        }
        .stable-dropdown:hover,
        .stable-dropdown:focus,
        .stable-dropdown:checked {
            border-color: #405267;
            background-color: #17212b;
        }
        .stable-dropdown label,
        .stable-dropdown image {
            margin: 0;
            padding: 0;
            border: 0 solid transparent;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            text-shadow: none;
            -gtk-icon-shadow: none;
            transform: none;
            transition: none;
        }
        .stable-dropdown-option,
        .stable-dropdown-option:hover,
        .stable-dropdown-option:focus,
        .stable-dropdown-option:active,
        .stable-dropdown-option:checked,
        .stable-dropdown-option.hover {
            background: transparent;
            background-color: transparent;
            background-image: none;
            color: #d7dce2;
            min-height: 34px;
            min-width: 220px;
            padding: 0 10px;
            margin: 0;
            border: 0 solid transparent;
            border-radius: 6px;
            border-image: none;
            box-shadow: none;
            outline-style: none;
            outline-width: 0;
            transform: none;
            transition: none;
        }
        .stable-dropdown-option:hover,
        .stable-dropdown-option:focus,
        .stable-dropdown-option.hover {
            background: #24303b;
            background-color: #24303b;
        }
        .stable-dropdown-option label,
        .stable-dropdown-option.hover label,
        .stable-dropdown-option:hover label {
            background: transparent;
            background-color: transparent;
            color: #d7dce2;
            margin: 0;
            padding: 0;
            border: none;
            box-shadow: none;
            text-shadow: none;
            transform: none;
            transition: none;
        }
        combobox popover > contents,
        combobox popover listbox,
        combobox popover listbox row,
        combobox popover modelbutton,
        combobox popover button {
            margin: 0;
            border: 0 solid transparent;
            box-shadow: none;
            transform: none;
            transition: none;
        }
        combobox popover listbox row,
        combobox popover listbox row:hover,
        combobox popover modelbutton,
        combobox popover modelbutton:hover {
            padding: 6px 10px;
        }
        entry selection, text selection, textview selection {
            background-color: #35506b;
            color: #f5f7fa;
        }
        label, checkbutton, checkbutton label {
            color: #d7dce2;
        }
        notebook > header, notebook > header tabs {
            background-color: #0f1419;
            border-bottom: 1px solid #242a30;
        }
        notebook tab, stackswitcher button {
            background-color: #161c22;
            color: #cfd6de;
            border: 1px solid #2b333c;
            border-radius: 6px;
        }
        notebook tab:checked, stackswitcher button:checked {
            background-color: #243344;
        }
        .session-tab-strip {
            background-color: #0f1419;
            border-bottom: 1px solid #242a30;
            padding: 6px 10px;
        }
        .empty-workspace {
            color: #d7dce2;
            padding: 28px;
            opacity: 1;
        }
        .empty-workspace-icon {
            color: #5f6c78;
            opacity: 0.78;
            margin-bottom: 2px;
        }
        .empty-workspace-title {
            color: #eef3f8;
            font-size: 1.18em;
            font-weight: 700;
        }
        .empty-workspace-subtitle {
            color: #8e98a4;
            font-size: 0.92em;
        }
        .empty-workspace-actions {
            margin-top: 4px;
        }
        .empty-primary-action {
            background-color: #1f74d2;
            border-color: #2582ea;
            color: #f6fbff;
            min-height: 36px;
            padding: 0 16px;
        }
        .empty-primary-action:hover {
            background-color: #2682ea;
            border-color: #3890f5;
        }
        .empty-primary-action:active {
            background-color: #1765bc;
            border-color: #2478db;
        }
        .empty-secondary-action {
            min-height: 36px;
            padding: 0 14px;
        }
        .dialog-content {
            background-color: #0c1014;
            padding: 16px;
        }
        .dialog-title,
        .settings-page-title {
            color: #eef3f8;
            font-size: 1.2em;
            font-weight: 700;
        }
        .dialog-subtitle,
        .settings-page-subtitle {
            color: #8e98a4;
            font-size: 0.9em;
        }
        .dialog-actions,
        .settings-actions {
            margin-top: 4px;
        }
        .new-session-form {
            background-color: #10161d;
            border: 1px solid #263341;
            border-radius: 10px;
            padding: 12px;
        }
        .protocol-switcher {
            margin-bottom: 8px;
        }
        .protocol-switcher button {
            min-height: 34px;
        }
        .protocol-page {
            background-color: transparent;
            padding: 2px;
        }
        .form-grid {
            margin-top: 4px;
        }
        .form-label {
            color: #b9c0c9;
            font-weight: 600;
        }
        .settings-page {
            background-color: #0c1014;
            padding: 26px;
        }
        .settings-page-content {
            min-width: 680px;
            padding: 0;
        }
        .settings-card {
            background-color: #10161d;
            border: 1px solid #263341;
            border-radius: 10px;
            padding: 16px;
            margin-bottom: 12px;
        }
        .settings-card-title {
            color: #eef3f8;
            font-weight: 700;
            margin-bottom: 6px;
        }
        .settings-expander {
            background-color: #10161d;
            border: 1px solid #263341;
            border-radius: 10px;
            padding: 10px 12px;
            margin-bottom: 12px;
        }
        .settings-expander > title {
            padding: 4px 0;
        }
        .session-tab-controls {
            border: 1px solid #2b333c;
            border-radius: 8px;
        }
        .session-strip-button,
        .session-strip-button:hover,
        .session-strip-button:active,
        .session-strip-button:checked {
            min-width: 36px;
            min-height: 36px;
            border: none;
            border-radius: 0;
            background: #1b2026;
            background-color: #1b2026;
            padding: 0;
        }
        .session-strip-button + .session-strip-button {
            border-left: 1px solid #2b333c;
        }
        .session-tabs-scroll, .session-tabs-scroll viewport {
            background: transparent;
            background-color: transparent;
        }
        .session-tabs-box {
            padding-left: 4px;
        }
        .session-tab-item {
            background-color: #161c22;
            border: 1px solid #2b333c;
            border-radius: 8px;
            padding: 0 4px 0 8px;
            box-shadow: none;
        }
        .session-tab-item.compact {
            padding: 0 2px 0 4px;
        }
        .session-tab-item.active {
            background-color: #22354a;
            border-color: #3e6d9f;
            box-shadow: inset 0 -2px 0 #2d8cff;
        }
        .session-tab-button,
        .session-tab-button:hover,
        .session-tab-button:active,
        .session-tab-button:checked {
            border: none;
            background: transparent;
            background-color: transparent;
            box-shadow: none;
            padding: 8px 6px;
        }
        paned > separator {
            background-color: #242a30;
            min-width: 1px;
            min-height: 1px;
            padding: 0;
            margin: 0;
        }
        .session-sidebar {
            background: #0e1114;
            background-color: #0e1114;
            border-right: 1px solid #242a30;
        }
        .sidebar-nav {
            min-width: 48px;
        }
        .sidebar-nav-button,
        .sidebar-nav-button:hover,
        .sidebar-nav-button:active,
        .sidebar-nav-button:checked {
            min-width: 44px;
            min-height: 44px;
            margin: 0;
            padding: 0;
            border: 1px solid transparent;
            border-radius: 10px;
            box-shadow: none;
            font-size: 1.18em;
        }
        .sidebar-nav-button image {
            margin: 0;
            padding: 0;
        }
        .sidebar-nav-button:hover {
            background-color: #172330;
            border-color: transparent;
        }
        .sidebar-nav-button.active,
        .sidebar-nav-button.active:hover {
            background-color: #1c3d5f;
            border-color: #275b8d;
            color: #eef3f8;
        }
        .sidebar-panel {
            min-width: 0;
        }
        .sidebar-panel-title {
            font-weight: 600;
            margin-bottom: 4px;
        }
        .sftp-search {
            margin-bottom: 2px;
        }
        .sftp-path {
            color: #aeb7c1;
            padding: 2px 2px 6px 2px;
        }
        .sftp-status {
            color: #aeb7c1;
            font-size: 0.9em;
        }
        .sidebar-scroll, .sidebar-scroll viewport, .profiles-list {
            background: #0f1419;
            background-color: #0f1419;
        }
        .profiles-list row {
            border-radius: 8px;
            margin: 2px 0;
        }
        .profiles-list row:selected {
            background-color: #1f3f5f;
        }
        .profiles-list row:hover {
            background-color: #172330;
        }
        .sftp-entry-row {
            padding: 0;
        }
        .sftp-entry-row-compact {
            min-width: 0;
        }
        .sftp-entry-icon {
            color: #7ab3ff;
        }
        .sftp-file-row .sftp-entry-icon {
            color: #8b97a3;
        }
        .sftp-entry-detail {
            font-size: 0.85em;
        }
        .app-status-bar {
            background-color: #0e1114;
            border-top: 1px solid #242a30;
            padding: 0 10px;
            min-height: 34px;
        }
        .host-info {
            color: #b9c0c9;
            padding: 0;
        }
        .status-label {
            color: #8e98a4;
            padding: 0;
        }
        .dim-label {
            color: #8e98a4;
            font-size: 0.88em;
        }
        "#,
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn build_modal_window(
    parent: &impl gtk4::prelude::IsA<Window>,
    title: &str,
    default_width: i32,
    default_height: i32,
) -> Window {
    let parent_window = parent.as_ref().clone();
    let win = Window::builder()
        .title(title)
        .transient_for(parent)
        .modal(true)
        .default_width(default_width)
        .default_height(default_height)
        .build();

    if let Some(app) = parent_window.application() {
        win.set_application(Some(&app));
    }
    win.set_destroy_with_parent(true);
    win.add_css_class("dialog-window");

    win.connect_close_request(move |_| {
        let parent_window = parent_window.clone();
        glib::idle_add_local_once(move || {
            parent_window.present();
            parent_window.grab_focus();
        });
        glib::Propagation::Proceed
    });

    win
}

fn build_sidebar_nav_button(icon_name: &str, tooltip: &str) -> Button {
    let button = Button::new();
    let image = Image::from_icon_name(icon_name);
    button.set_child(Some(&image));
    button.set_tooltip_text(Some(tooltip));
    button.set_has_frame(false);
    button.add_css_class("sidebar-nav-button");
    button
}

fn build_empty_workspace(language: &AppLanguage) -> (GtkBox, Button, Button) {
    let root = GtkBox::new(Orientation::Vertical, 14);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_halign(gtk4::Align::Center);
    root.set_valign(gtk4::Align::Center);
    root.add_css_class("empty-workspace");

    let icon = Image::from_icon_name("utilities-terminal-symbolic");
    icon.set_pixel_size(54);
    icon.add_css_class("empty-workspace-icon");

    let title = Label::new(Some(tr(
        language,
        "选择一个入口开始",
        "Choose a starting point",
    )));
    title.add_css_class("empty-workspace-title");

    let subtitle = Label::new(Some(tr(
        language,
        "打开本地终端、连接已保存会话，或创建一个新的远程配置。",
        "Open a local terminal, connect a saved session, or create a new remote profile.",
    )));
    subtitle.set_wrap(true);
    subtitle.set_justify(gtk4::Justification::Center);
    subtitle.set_max_width_chars(48);
    subtitle.add_css_class("empty-workspace-subtitle");

    let actions = GtkBox::new(Orientation::Horizontal, 8);
    actions.set_halign(gtk4::Align::Center);
    actions.add_css_class("empty-workspace-actions");

    let open_local_btn = Button::with_label(tr(language, "打开本地终端", "Open Local Terminal"));
    open_local_btn.add_css_class("suggested-action");
    open_local_btn.add_css_class("empty-primary-action");

    let new_session_btn = Button::with_label(tr(language, "新建会话", "New Session"));
    new_session_btn.add_css_class("empty-secondary-action");

    actions.append(&open_local_btn);
    actions.append(&new_session_btn);

    root.append(&icon);
    root.append(&title);
    root.append(&subtitle);
    root.append(&actions);

    (root, open_local_btn, new_session_btn)
}

fn sync_workspace_state(state: &AppState) {
    if state.notebook.n_pages() == 0 {
        state
            .workspace_stack
            .set_visible_child_name(WORKSPACE_PAGE_EMPTY);
    } else {
        state
            .workspace_stack
            .set_visible_child_name(WORKSPACE_PAGE_SESSIONS);
    }
}

fn local_terminals_cache_path() -> PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shell")
        .join("local-terminals-cache.json")
}

fn load_or_discover_local_terminals(settings: &AppSettings) -> Vec<LocalTerminalProfile> {
    let toolchain = active_builtin_toolchain(&settings.builtin_tools);
    if let Ok(cached) = load_cached_local_terminals()
        && !cached.is_empty()
        && cached_local_terminals_match_runtime(&cached, toolchain.is_some())
    {
        return cached;
    }

    let terminals = discover_local_terminals(toolchain.as_ref());
    let _ = save_local_terminal_cache(&terminals);
    terminals
}

fn cached_local_terminals_match_runtime(
    terminals: &[LocalTerminalProfile],
    toolchain_available: bool,
) -> bool {
    let has_toolchain_profile = terminals
        .iter()
        .any(|terminal| terminal.toolchain.is_some());
    has_toolchain_profile == toolchain_available
}

fn load_cached_local_terminals() -> anyhow::Result<Vec<LocalTerminalProfile>> {
    let path = local_terminals_cache_path();
    if !path.exists() {
        anyhow::bail!("cache missing")
    }

    let content = fs::read_to_string(path)?;
    let cache: LocalTerminalCache = serde_json::from_str(&content)?;
    if cache.version != LOCAL_TERMINALS_CACHE_VERSION {
        anyhow::bail!("cache version mismatch")
    }

    Ok(cache.terminals)
}

fn save_local_terminal_cache(terminals: &[LocalTerminalProfile]) -> anyhow::Result<()> {
    let path = local_terminals_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cache = LocalTerminalCache {
        version: LOCAL_TERMINALS_CACHE_VERSION,
        terminals: terminals.to_vec(),
    };
    fs::write(path, serde_json::to_string_pretty(&cache)?)?;
    Ok(())
}

fn discover_local_terminals(toolchain: Option<&BuiltinToolchain>) -> Vec<LocalTerminalProfile> {
    #[cfg(windows)]
    {
        discover_local_terminals_windows(toolchain)
    }

    #[cfg(not(windows))]
    {
        let _ = toolchain;
        discover_local_terminals_unix()
    }
}

#[cfg(windows)]
fn discover_local_terminals_windows(
    toolchain: Option<&BuiltinToolchain>,
) -> Vec<LocalTerminalProfile> {
    let preferred_shell = std::env::var("ComSpec")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let cmd_paths = where_command_paths("cmd.exe");
    let powershell_paths = where_command_paths("powershell.exe");
    let pwsh_paths = where_command_paths("pwsh.exe");
    let bash_paths = where_command_paths("bash.exe");
    let clink_paths = where_command_paths("clink.bat")
        .into_iter()
        .chain(where_command_paths("clink.cmd"))
        .chain(where_command_paths("clink.exe"))
        .collect::<Vec<_>>();

    let mut terminals = Vec::new();
    let mut seen = HashSet::new();

    if let Some(path) = powershell_paths.first() {
        push_local_terminal(
            &mut terminals,
            &mut seen,
            LocalTerminalProfile {
                id: format!("powershell:{}", path.display()),
                title: "Windows PowerShell".to_string(),
                detail: path.display().to_string(),
                program: path.display().to_string(),
                args: vec!["-NoLogo".to_string()],
                toolchain: None,
                icon_name: "utilities-terminal-symbolic".to_string(),
                priority: if preferred_shell.contains("powershell") {
                    0
                } else {
                    20
                },
            },
        );
    }

    if let Some(path) = pwsh_paths.first() {
        push_local_terminal(
            &mut terminals,
            &mut seen,
            LocalTerminalProfile {
                id: format!("pwsh:{}", path.display()),
                title: "PowerShell 7".to_string(),
                detail: path.display().to_string(),
                program: path.display().to_string(),
                args: vec!["-NoLogo".to_string()],
                toolchain: None,
                icon_name: "utilities-terminal-symbolic".to_string(),
                priority: if preferred_shell.contains("pwsh") {
                    0
                } else {
                    24
                },
            },
        );
    }

    if let (Some(cmd_path), Some(clink_path)) = (cmd_paths.first(), clink_paths.first()) {
        push_local_terminal(
            &mut terminals,
            &mut seen,
            LocalTerminalProfile {
                id: format!("cmd-clink:{}", clink_path.display()),
                title: "CMD (clink)".to_string(),
                detail: cmd_path.display().to_string(),
                program: cmd_path.display().to_string(),
                args: vec![
                    "/k".to_string(),
                    format!("\"{}\" inject", clink_path.display()),
                ],
                toolchain: None,
                icon_name: "utilities-terminal-symbolic".to_string(),
                priority: if preferred_shell.contains("cmd.exe") {
                    8
                } else {
                    28
                },
            },
        );
    }

    if let Some(path) = cmd_paths.first() {
        push_local_terminal(
            &mut terminals,
            &mut seen,
            LocalTerminalProfile {
                id: format!("cmd:{}", path.display()),
                title: "CMD (stock)".to_string(),
                detail: path.display().to_string(),
                program: path.display().to_string(),
                args: Vec::new(),
                toolchain: None,
                icon_name: "utilities-terminal-symbolic".to_string(),
                priority: if preferred_shell.contains("cmd.exe") {
                    0
                } else {
                    30
                },
            },
        );
    }

    if let Some(path) = bash_paths
        .iter()
        .find(|path| {
            path.to_string_lossy()
                .to_ascii_lowercase()
                .contains("git\\bin\\bash.exe")
        })
        .or_else(|| bash_paths.first())
    {
        push_local_terminal(
            &mut terminals,
            &mut seen,
            LocalTerminalProfile {
                id: format!("git-bash:{}", path.display()),
                title: "Git Bash".to_string(),
                detail: path.display().to_string(),
                program: path.display().to_string(),
                args: vec!["--login".to_string(), "-i".to_string()],
                toolchain: None,
                icon_name: "utilities-terminal-symbolic".to_string(),
                priority: 40,
            },
        );
    }

    let msys_script = discover_msys2_shell_script();
    if let (Some(script), Some(cmd_path)) = (msys_script, cmd_paths.first()) {
        for (title, flavor, priority) in [
            ("MSYS2 (CLANG64)", "-clang64", 50),
            ("MSYS2 (MINGW64)", "-mingw64", 52),
            ("MSYS2 (MSYS)", "-msys", 54),
            ("MSYS2 (UCRT64)", "-ucrt64", 56),
        ] {
            push_local_terminal(
                &mut terminals,
                &mut seen,
                LocalTerminalProfile {
                    id: format!("msys2:{flavor}:{}", script.display()),
                    title: title.to_string(),
                    detail: script.display().to_string(),
                    program: cmd_path.display().to_string(),
                    args: vec![
                        "/c".to_string(),
                        script.display().to_string(),
                        "-defterm".to_string(),
                        "-here".to_string(),
                        "-no-start".to_string(),
                        flavor.to_string(),
                    ],
                    toolchain: None,
                    icon_name: "utilities-terminal-symbolic".to_string(),
                    priority,
                },
            );
        }
    }

    if let Some(toolchain) = toolchain {
        push_builtin_toolchain_terminal(&mut terminals, &mut seen, toolchain);
    }

    terminals.sort_by_key(|terminal| (terminal.priority, terminal.title.clone()));
    terminals
}

#[cfg(windows)]
fn push_builtin_toolchain_terminal(
    terminals: &mut Vec<LocalTerminalProfile>,
    seen: &mut HashSet<String>,
    toolchain: &BuiltinToolchain,
) {
    let Some(bash) = toolchain.command_path("bash") else {
        return;
    };

    push_local_terminal(
        terminals,
        seen,
        LocalTerminalProfile {
            id: format!("shell-tools-bash:{}", toolchain.root.display()),
            title: "Shell Tools (Bash)".to_string(),
            detail: toolchain.root.display().to_string(),
            program: bash.display().to_string(),
            args: vec!["--noprofile".to_string(), "-i".to_string()],
            toolchain: Some(LocalTerminalToolchain::ShellToolsBash),
            icon_name: "utilities-terminal-symbolic".to_string(),
            priority: 10,
        },
    );
}

#[cfg(not(windows))]
fn discover_local_terminals_unix() -> Vec<LocalTerminalProfile> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    vec![LocalTerminalProfile {
        id: shell.clone(),
        title: Path::new(&shell)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Shell")
            .to_string(),
        detail: shell.clone(),
        program: shell,
        args: vec!["-i".to_string()],
        toolchain: None,
        icon_name: "utilities-terminal-symbolic".to_string(),
        priority: 0,
    }]
}

#[cfg(windows)]
fn discover_msys2_shell_script() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(root) = std::env::var("MSYS2_ROOT") {
        candidates.push(PathBuf::from(root).join("msys2_shell.cmd"));
    }
    candidates.push(PathBuf::from(r"C:\msys64\msys2_shell.cmd"));
    candidates.push(PathBuf::from(r"C:\tools\msys64\msys2_shell.cmd"));
    if let Ok(current_dir) = std::env::current_dir() {
        candidates.push(current_dir.join(".msys64").join("msys2_shell.cmd"));
    }
    candidates.into_iter().find(|path| path.exists())
}

fn where_command_paths(command: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    let output = Command::new("where.exe").arg(command).output();
    #[cfg(not(windows))]
    let output = Command::new("which").arg(command).output();

    output
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn push_local_terminal(
    terminals: &mut Vec<LocalTerminalProfile>,
    seen: &mut HashSet<String>,
    terminal: LocalTerminalProfile,
) {
    if seen.insert(terminal.id.clone()) {
        terminals.push(terminal);
    }
}

fn active_builtin_toolchain(settings: &BuiltinToolsSettings) -> Option<BuiltinToolchain> {
    if !settings.enabled {
        return None;
    }
    BuiltinToolchain::discover().toolchain
}

fn to_platform_path_priority(priority: &BuiltinToolsPathPriority) -> ToolchainPathPriority {
    match priority {
        BuiltinToolsPathPriority::SystemFirst => ToolchainPathPriority::SystemFirst,
        BuiltinToolsPathPriority::ToolchainFirst => ToolchainPathPriority::ToolchainFirst,
    }
}

fn local_terminal_launch_options(
    settings: &AppSettings,
    terminal: &LocalTerminalProfile,
) -> PtyLaunchOptions {
    let Some(toolchain) = active_builtin_toolchain(&settings.builtin_tools) else {
        return PtyLaunchOptions::default();
    };

    if terminal.toolchain == Some(LocalTerminalToolchain::ShellToolsBash) {
        return toolchain.launch_options(ToolchainPathPriority::ToolchainFirst);
    }

    if settings.builtin_tools.inject_into_system_shells {
        return toolchain.launch_options(to_platform_path_priority(
            &settings.builtin_tools.path_priority,
        ));
    }

    PtyLaunchOptions::default()
}

fn preferred_local_terminal(state: &AppState) -> Option<LocalTerminalProfile> {
    state.local_terminals.first().cloned()
}

fn open_preferred_local_terminal(state: &AppState) {
    let language = state.app_settings.borrow().language.clone();
    let Some(profile) = preferred_local_terminal(state) else {
        state.status.set_text(tr(
            &language,
            "未在此系统发现本地终端。",
            "No local terminals were discovered on this system.",
        ));
        return;
    };

    match spawn_local_tab(state, &profile) {
        Ok(connection) => {
            reset_host_stats(state, host_idle_text(&language));
            state.connections.borrow_mut().locals.push(connection);
            state.status.set_text(&format!(
                "{} {}",
                tr(&language, "已打开", "Opened"),
                profile.title
            ));
        }
        Err(err) => state
            .status
            .set_text(&format!("Failed to start {}: {err}", profile.title)),
    }
}

fn schedule_debug_local_terminal_autostart(state: &AppState) {
    let count = std::env::var("SHELL_APP_DEBUG_OPEN_LOCAL_TABS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if count == 0 || state.local_terminals.is_empty() {
        return;
    }

    let terminals = state
        .local_terminals
        .iter()
        .cloned()
        .cycle()
        .take(count)
        .collect::<Vec<_>>();
    let state_for_open = state.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        for terminal in terminals.iter().cloned() {
            match spawn_local_tab(&state_for_open, &terminal) {
                Ok(connection) => {
                    reset_host_stats(
                        &state_for_open,
                        "Host: -    CPU: -    Mem: -    Net: -    Uptime: -",
                    );
                    state_for_open
                        .connections
                        .borrow_mut()
                        .locals
                        .push(connection);
                }
                Err(err) => {
                    state_for_open
                        .status
                        .set_text(&format!("Failed to start {}: {err}", terminal.title));
                    break;
                }
            }
        }
        ControlFlow::Break
    });
}

fn schedule_debug_tab_autoclose(state: &AppState) {
    let delay = std::env::var("SHELL_APP_DEBUG_CLOSE_TABS_AFTER_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if delay == 0 {
        return;
    }

    let notebook = state.notebook.clone();
    glib::timeout_add_local(Duration::from_millis(delay), move || {
        while notebook.n_pages() > 0 {
            notebook.remove_page(Some(0));
        }
        ControlFlow::Break
    });
}

fn show_local_terminal_popover(state: &AppState, anchor: &Button) {
    let popover = Popover::new();
    popover.set_has_arrow(false);
    popover.set_position(PositionType::Bottom);
    popover.set_offset(0, 8);
    popover.set_parent(anchor);

    let root = GtkBox::new(Orientation::Vertical, 2);
    root.add_css_class("context-menu-list");

    for terminal in state.local_terminals.iter().cloned() {
        let button = Button::new();
        button.set_has_frame(false);
        button.add_css_class("profile-actions-button");
        button.add_css_class("context-menu-button");

        let row = GtkBox::new(Orientation::Horizontal, 8);
        let icon = Image::from_icon_name(&terminal.icon_name);
        let text = GtkBox::new(Orientation::Vertical, 2);
        let title = Label::new(Some(&terminal.title));
        title.set_xalign(0.0);
        let detail = Label::new(Some(&terminal.detail));
        detail.set_xalign(0.0);
        detail.add_css_class("dim-label");
        text.append(&title);
        text.append(&detail);
        row.append(&icon);
        row.append(&text);
        button.set_child(Some(&row));

        let popover_for_button = popover.clone();
        let state_for_button = state.clone();
        button.connect_clicked(move |_| {
            popover_for_button.popdown();
            let language = state_for_button.app_settings.borrow().language.clone();
            match spawn_local_tab(&state_for_button, &terminal) {
                Ok(connection) => {
                    reset_host_stats(&state_for_button, host_idle_text(&language));
                    state_for_button
                        .connections
                        .borrow_mut()
                        .locals
                        .push(connection);
                    state_for_button.status.set_text(&format!(
                        "{} {}",
                        tr(&language, "已打开", "Opened"),
                        terminal.title
                    ));
                }
                Err(err) => state_for_button
                    .status
                    .set_text(&format!("Failed to start {}: {err}", terminal.title)),
            }
        });
        root.append(&button);
    }

    popover.set_child(Some(&root));
    register_active_popover(state, &popover);
    popover.popup();
}

fn remove_local_shell_profiles(document: &mut ProfilesDocument) -> bool {
    let previous_len = document.profiles.len();
    document
        .profiles
        .retain(|profile| !matches!(profile.protocol, ProtocolKind::LocalShell));
    previous_len != document.profiles.len()
}

fn set_sidebar_nav_active(button: &Button, active: bool) {
    if active {
        button.add_css_class("active");
    } else {
        button.remove_css_class("active");
    }
}

fn show_sidebar_section(state: &AppState, name: &str) {
    close_active_popover(state);
    state.sidebar_nav.stack.set_visible_child_name(name);
    set_sidebar_nav_active(&state.sidebar_nav.sessions_button, name == "sessions");
    set_sidebar_nav_active(&state.sidebar_nav.sftp_button, name == "sftp");
}

fn close_active_popover(state: &AppState) {
    let popover = state.active_popover.borrow_mut().take();
    if let Some(popover) = popover {
        popover.popdown();
    }
}

fn register_active_popover(state: &AppState, popover: &Popover) {
    close_active_popover(state);
    popover.add_css_class("app-menu-popover");
    popover.set_autohide(true);
    *state.active_popover.borrow_mut() = Some(popover.clone());

    let active_popover = Rc::clone(&state.active_popover);
    let popover_for_closed = popover.clone();
    popover.connect_closed(move |_| {
        popover_for_closed.unparent();
        let should_clear = active_popover
            .borrow()
            .as_ref()
            .is_some_and(|current| current.as_ptr() == popover_for_closed.as_ptr());
        if should_clear {
            *active_popover.borrow_mut() = None;
        }
    });
}

fn schedule_ui_action(action: impl FnOnce() + 'static) {
    let action = Rc::new(RefCell::new(Some(action)));
    glib::idle_add_local(move || {
        if let Some(action) = action.borrow_mut().take() {
            action();
        }
        ControlFlow::Break
    });
}

#[derive(Default)]
struct SessionHandles {
    // UI 闭包会持有自己的 `Rc` 克隆；这个注册表再持有一份，保证活动协议对象不会在
    // 页面关闭前被释放。页面关闭后，`prune_session_handles` 观察到只剩注册表克隆时
    // 再清理。
    locals: Vec<Rc<LocalShellConnection>>,
    ssh: Vec<Rc<SshConnection>>,
    telnet: Vec<Rc<TelnetConnection>>,
    serial: Vec<Rc<SerialConnection>>,
    ftp: Vec<Rc<FtpConnection>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LocalTerminalProfile {
    id: String,
    title: String,
    detail: String,
    program: String,
    args: Vec<String>,
    #[serde(default)]
    toolchain: Option<LocalTerminalToolchain>,
    icon_name: String,
    priority: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LocalTerminalToolchain {
    ShellToolsBash,
}

#[derive(Debug, Serialize, Deserialize)]
struct LocalTerminalCache {
    version: u32,
    terminals: Vec<LocalTerminalProfile>,
}

#[derive(Clone)]
struct SessionTabStrip {
    // 显示在隐藏 GTK Notebook 上方的自定义标签栏。它提供稳定的关闭按钮几何和横向
    // 溢出控制，解决默认 notebook tab 在大量终端会话下不可控的问题。
    container: GtkBox,
    controls: GtkBox,
    tabs_scroll: ScrolledWindow,
    tabs_box: GtkBox,
    add_button: Button,
    dropdown_button: Button,
    width_cap: Rc<RefCell<Option<i32>>>,
}

#[derive(Clone)]
struct PageTab {
    // 使用弱引用，避免 GTK 移除页面后仍被标签元数据强行保活。
    widget: glib::WeakRef<Widget>,
    title: String,
}

#[derive(Clone)]
struct SidebarNav {
    stack: Stack,
    sessions_button: Button,
    sftp_button: Button,
}

#[derive(Clone)]
struct AppChrome {
    new_session_button: Button,
    settings_button: Button,
    sidebar_title: Label,
}

#[derive(Clone)]
struct SftpSidebar {
    container: GtkBox,
    compact: bool,
    title_label: Label,
    search_entry: Entry,
    path_label: Label,
    list: ListBox,
    scroll: ScrolledWindow,
    status_label: Label,
    progress_bar: ProgressBar,
    refresh_button: Button,
    upload_button: Button,
    download_button: Button,
    mkdir_button: Button,
    rename_button: Button,
    delete_button: Button,
    session: Rc<RefCell<Option<SftpSession>>>,
    config_key: Rc<RefCell<Option<String>>>,
    clipboard: Rc<RefCell<Option<SftpClipboard>>>,
}

#[derive(Clone)]
struct SftpClipboard {
    config_key: String,
    name: String,
    source_path: String,
    is_dir: bool,
}

#[derive(Clone)]
struct PageContext {
    // 当前页面可复用的元数据。SFTP 快捷入口和 SSH 重连读取这里，而不是从 widget
    // 反推状态。
    widget: glib::WeakRef<Widget>,
    profile: ConnectionProfile,
    runtime_password: Option<String>,
}

#[derive(Clone)]
struct SshReconnectSession {
    // 每个 SSH 页签独立的状态。重连时只替换底层连接，用户看到的终端视图、页签标题
    // 和页面上下文保持不变。
    view: GtkTerminalView,
    page: DrawingArea,
    profile: ConnectionProfile,
    runtime_password: Option<String>,
    connection: Rc<RefCell<Option<Rc<SshConnection>>>>,
    disconnected: Rc<Cell<bool>>,
    enter_presses: Rc<Cell<u8>>,
    reconnecting: Rc<Cell<bool>>,
    last_size: Rc<Cell<TerminalSize>>,
}

struct PageShutdown {
    widget: glib::WeakRef<Widget>,
    action: Rc<dyn Fn()>,
}

#[derive(Clone)]
struct AppState {
    // GTK 回调捕获的唯一共享 UI 状态对象。尽量不要把协议逻辑塞进这里；通过辅助函数
    // 和协议适配包保持回调短小、可测试。
    window: ApplicationWindow,
    notebook: Notebook,
    workspace_stack: Stack,
    status: Label,
    host_info: Label,
    profiles_list: ListBox,
    profiles_doc: Rc<RefCell<ProfilesDocument>>,
    profile_store: ProfileStore,
    app_settings: Rc<RefCell<AppSettings>>,
    terminal_appearance: TerminalAppearance,
    terminal_widgets: Rc<RefCell<Vec<glib::WeakRef<DrawingArea>>>>,
    page_tabs: Rc<RefCell<Vec<PageTab>>>,
    page_contexts: Rc<RefCell<Vec<PageContext>>>,
    page_shutdowns: Rc<RefCell<Vec<PageShutdown>>>,
    connections: Rc<RefCell<SessionHandles>>,
    host_stats_poller: Rc<RefCell<Option<glib::SourceId>>>,
    window_size_cap: Rc<RefCell<Option<(i32, i32)>>>,
    active_popover: Rc<RefCell<Option<Popover>>>,
    local_terminals: Rc<Vec<LocalTerminalProfile>>,
    chrome: AppChrome,
    session_tab_strip: SessionTabStrip,
    sidebar_nav: SidebarNav,
    sftp_sidebar: SftpSidebar,
}

type OutputObserver = Rc<RefCell<Option<Box<dyn FnMut(&[u8])>>>>;
// 可选回调：类终端适配器用它把 EOF/error 报告给页面专属状态机。SSH 使用它来启用
// 双回车重连。
type DisconnectObserver = Rc<dyn Fn(&str)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SshDisconnectedInputAction {
    Forward,
    Hint,
    Reconnect,
    Ignore,
}

#[derive(Clone)]
struct PasswordFields {
    username: Entry,
    password: Entry,
    remember: CheckButton,
}

#[derive(Clone)]
struct KeyFields {
    username: Entry,
    key_path: Entry,
}

#[derive(Clone)]
struct StableComboOption {
    id: String,
    label: String,
}

type StableComboChanged = Rc<RefCell<Vec<Box<dyn Fn(&StableComboBox)>>>>;

#[derive(Clone)]
struct StableComboBox {
    button: Button,
    value_label: Label,
    options: Rc<RefCell<Vec<StableComboOption>>>,
    active_id: Rc<RefCell<Option<String>>>,
    changed_handlers: StableComboChanged,
}

#[derive(Clone)]
struct AuthFields {
    mode: StableComboBox,
    stack: Stack,
    keyboard_username: Entry,
    password: PasswordFields,
    private_key: KeyFields,
}

#[derive(Clone)]
struct NewSessionForm {
    protocol_stack: Stack,
    ssh_name: Entry,
    ssh_host: Entry,
    ssh_port: Entry,
    ssh_auth: AuthFields,
    sftp_name: Entry,
    sftp_host: Entry,
    sftp_port: Entry,
    sftp_auth: AuthFields,
    ftp_name: Entry,
    ftp_host: Entry,
    ftp_port: Entry,
    ftp_username: Entry,
    ftp_password: Entry,
    ftp_remember: CheckButton,
    telnet_name: Entry,
    telnet_host: Entry,
    telnet_port: Entry,
    serial_name: Entry,
    serial_port: Entry,
    serial_baud: Entry,
}

#[derive(Clone)]
struct PendingSecret {
    reference: CredentialsRef,
    password: String,
}

#[derive(Clone)]
struct ProfileSubmission {
    profile: ConnectionProfile,
    runtime_password: Option<String>,
    pending_secret: Option<PendingSecret>,
}

fn connect_profiles_context_menu(state: &AppState) {
    let click = GestureClick::new();
    click.set_button(3);
    let state_for_click = state.clone();
    click.connect_released(move |_, _presses, x, y| {
        if let Some(row) = state_for_click.profiles_list.row_at_y(y as i32) {
            state_for_click.profiles_list.select_row(Some(&row));
            show_profile_actions_popover(
                &state_for_click,
                row.index() as usize,
                gdk::Rectangle::new(x as i32, y as i32, 220, 20),
            );
        }
    });
    state.profiles_list.add_controller(click);
}

fn show_profile_actions_popover(state: &AppState, index: usize, anchor: gdk::Rectangle) {
    let popover = Popover::new();
    popover.set_has_arrow(false);
    popover.set_position(PositionType::Bottom);
    popover.set_offset(8, 8);
    popover.set_parent(&state.profiles_list);
    popover.set_pointing_to(Some(&anchor));

    let root = GtkBox::new(Orientation::Vertical, 2);
    root.add_css_class("context-menu-list");

    let supports_sftp = state
        .profiles_doc
        .borrow()
        .profiles
        .get(index)
        .is_some_and(profile_supports_sftp_shortcut);

    let language = state.app_settings.borrow().language.clone();
    let connect_btn = Button::with_label(tr(&language, "连接", "Connect"));
    let connect_sftp_btn = Button::with_label(tr(&language, "连接 SFTP", "Connect SFTP"));
    let edit_btn = Button::with_label(tr(&language, "编辑", "Edit"));
    let delete_btn = Button::with_label(tr(&language, "删除", "Delete"));
    let cancel_btn = Button::with_label(tr(&language, "关闭", "Close"));
    connect_sftp_btn.set_sensitive(supports_sftp);
    for button in [
        &connect_btn,
        &connect_sftp_btn,
        &edit_btn,
        &delete_btn,
        &cancel_btn,
    ] {
        button.add_css_class("profile-actions-button");
        button.add_css_class("context-menu-button");
    }
    root.append(&connect_btn);
    root.append(&connect_sftp_btn);
    root.append(&edit_btn);
    root.append(&delete_btn);
    root.append(&cancel_btn);
    popover.set_child(Some(&root));

    let state_for_connect = state.clone();
    let popover_for_connect = popover.clone();
    connect_btn.connect_clicked(move |_| {
        popover_for_connect.popdown();
        let state_for_connect = state_for_connect.clone();
        schedule_ui_action(move || {
            if let Some(profile) = state_for_connect
                .profiles_doc
                .borrow()
                .profiles
                .get(index)
                .cloned()
            {
                connect_saved_profile(&state_for_connect, profile);
            }
        });
    });

    let state_for_sftp = state.clone();
    let popover_for_sftp = popover.clone();
    connect_sftp_btn.connect_clicked(move |_| {
        popover_for_sftp.popdown();
        let state_for_sftp = state_for_sftp.clone();
        schedule_ui_action(move || {
            if let Some(profile) = state_for_sftp
                .profiles_doc
                .borrow()
                .profiles
                .get(index)
                .cloned()
            {
                connect_saved_profile_sftp(&state_for_sftp, profile);
            }
        });
    });

    let state_for_edit = state.clone();
    let popover_for_edit = popover.clone();
    edit_btn.connect_clicked(move |_| {
        popover_for_edit.popdown();
        let state_for_edit = state_for_edit.clone();
        schedule_ui_action(move || {
            show_edit_session_window(&state_for_edit, index);
        });
    });

    let state_for_delete = state.clone();
    let popover_for_delete = popover.clone();
    delete_btn.connect_clicked(move |_| {
        popover_for_delete.popdown();
        let state_for_delete = state_for_delete.clone();
        schedule_ui_action(move || {
            delete_profile_at(&state_for_delete, index);
        });
    });

    let popover_for_cancel = popover.clone();
    cancel_btn.connect_clicked(move |_| {
        popover_for_cancel.popdown();
    });

    register_active_popover(state, &popover);
    popover.popup();
}

fn parse_port_value(value: &str, default_port: u16) -> u16 {
    value.trim().parse::<u16>().unwrap_or(default_port)
}

impl NewSessionForm {
    fn collect(&self) -> anyhow::Result<ProfileSubmission> {
        let active_protocol = self
            .protocol_stack
            .visible_child_name()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "ssh".to_string());

        match active_protocol.as_str() {
            "ssh" => {
                let host = require_entry_text("SSH host", &self.ssh_host)?;
                let mut profile = ConnectionProfile::new(
                    default_profile_name(&self.ssh_name, &format!("SSH {host}")),
                    ProtocolKind::Ssh,
                );
                profile.host = Some(host);
                profile.port = Some(parse_port_value(&self.ssh_port.text(), 22));
                let (auth, runtime_password, pending_secret) =
                    collect_auth(&profile, &self.ssh_auth, true)?;
                profile.auth = auth;
                Ok(ProfileSubmission {
                    profile,
                    runtime_password,
                    pending_secret,
                })
            }
            "sftp" => {
                let host = require_entry_text("SFTP host", &self.sftp_host)?;
                let mut profile = ConnectionProfile::new(
                    default_profile_name(&self.sftp_name, &format!("SFTP {host}")),
                    ProtocolKind::Sftp,
                );
                profile.host = Some(host);
                profile.port = Some(parse_port_value(&self.sftp_port.text(), 22));
                let (auth, runtime_password, pending_secret) =
                    collect_auth(&profile, &self.sftp_auth, false)?;
                profile.auth = auth;
                Ok(ProfileSubmission {
                    profile,
                    runtime_password,
                    pending_secret,
                })
            }
            "ftp" => {
                let host = require_entry_text("FTP host", &self.ftp_host)?;
                let username = default_entry_text(&self.ftp_username, "anonymous");
                let password = self.ftp_password.text().to_string();
                let mut profile = ConnectionProfile::new(
                    default_profile_name(&self.ftp_name, &format!("FTP {host}")),
                    ProtocolKind::Ftp,
                );
                profile.host = Some(host);
                profile.port = Some(parse_port_value(&self.ftp_port.text(), 21));
                let (password_ref, pending_secret) = plan_password_storage(
                    &profile,
                    &username,
                    &password,
                    self.ftp_remember.is_active(),
                );
                profile.auth = AuthConfig::Password {
                    username,
                    password_ref,
                };
                Ok(ProfileSubmission {
                    profile,
                    runtime_password: (!password.is_empty()).then_some(password),
                    pending_secret,
                })
            }
            "telnet" => {
                let host = require_entry_text("Telnet host", &self.telnet_host)?;
                let mut profile = ConnectionProfile::new(
                    default_profile_name(&self.telnet_name, &format!("Telnet {host}")),
                    ProtocolKind::Telnet,
                );
                profile.host = Some(host);
                profile.port = Some(parse_port_value(&self.telnet_port.text(), 23));
                Ok(ProfileSubmission {
                    profile,
                    runtime_password: None,
                    pending_secret: None,
                })
            }
            "serial" => {
                let serial_port = require_entry_text("Serial port", &self.serial_port)?;
                let mut profile = ConnectionProfile::new(
                    default_profile_name(&self.serial_name, &format!("Serial {serial_port}")),
                    ProtocolKind::Serial,
                );
                profile.serial_port = Some(serial_port);
                profile.baud_rate = Some(
                    self.serial_baud
                        .text()
                        .trim()
                        .parse::<u32>()
                        .unwrap_or(115_200),
                );
                Ok(ProfileSubmission {
                    profile,
                    runtime_password: None,
                    pending_secret: None,
                })
            }
            other => anyhow::bail!("Unsupported protocol page: {other}"),
        }
    }
}

fn show_new_session_window(state: &AppState) {
    let language = state.app_settings.borrow().language.clone();
    let win = build_modal_window(
        &state.window,
        tr(&language, "新增保存会话", "New Session"),
        560,
        560,
    );

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("dialog-content");

    let title = Label::new(Some(tr(&language, "新增保存会话", "New Session")));
    title.set_xalign(0.0);
    title.add_css_class("dialog-title");
    let subtitle = Label::new(Some(tr(
        &language,
        "选择协议并保存连接信息，之后可以从侧栏快速打开。",
        "Choose a protocol and save the connection details for quick access from the sidebar.",
    )));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.add_css_class("dialog-subtitle");
    root.append(&title);
    root.append(&subtitle);

    let (form_widget, form) = build_new_session_form(&language);
    root.append(&form_widget);

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.add_css_class("dialog-actions");
    let save_btn = Button::with_label(tr(&language, "保存", "Save"));
    let save_connect_btn = Button::with_label(tr(&language, "保存并连接", "Save & Connect"));
    let cancel_btn = Button::with_label(tr(&language, "取消", "Cancel"));
    actions.append(&cancel_btn);
    actions.append(&save_btn);
    actions.append(&save_connect_btn);
    root.append(&actions);

    win.set_child(Some(&root));

    let state_for_save = state.clone();
    let form_for_save = form.clone();
    let win_for_save = win.clone();
    save_btn.connect_clicked(move |_| match form_for_save.collect() {
        Ok(submission) => submit_profile(&state_for_save, submission, false, &win_for_save),
        Err(err) => state_for_save.status.set_text(&err.to_string()),
    });

    let state_for_connect = state.clone();
    let form_for_connect = form.clone();
    let win_for_connect = win.clone();
    save_connect_btn.connect_clicked(move |_| match form_for_connect.collect() {
        Ok(submission) => submit_profile(&state_for_connect, submission, true, &win_for_connect),
        Err(err) => state_for_connect.status.set_text(&err.to_string()),
    });

    let win_for_cancel = win.clone();
    cancel_btn.connect_clicked(move |_| {
        win_for_cancel.close();
    });

    win.present();
}

fn show_edit_session_window(state: &AppState, index: usize) {
    let language = state.app_settings.borrow().language.clone();
    let Some(existing) = state.profiles_doc.borrow().profiles.get(index).cloned() else {
        return;
    };

    if matches!(existing.protocol, ProtocolKind::LocalShell) {
        state.status.set_text(
            "Local terminals are now opened from the + menu and are no longer saved as sessions.",
        );
        return;
    }

    let win = build_modal_window(
        &state.window,
        &format!("{} {}", tr(&language, "编辑", "Edit"), existing.name),
        560,
        560,
    );

    let root = GtkBox::new(Orientation::Vertical, 12);
    root.add_css_class("dialog-content");

    let title = Label::new(Some(&format!(
        "{} {}",
        tr(&language, "编辑", "Edit"),
        existing.name
    )));
    title.set_xalign(0.0);
    title.add_css_class("dialog-title");
    let subtitle = Label::new(Some(tr(
        &language,
        "更新保存的连接信息，保存后侧栏会立即刷新。",
        "Update the saved connection details. The sidebar refreshes after saving.",
    )));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.add_css_class("dialog-subtitle");
    root.append(&title);
    root.append(&subtitle);

    let (form_widget, form) = build_new_session_form(&language);
    populate_form_from_profile(&form, &existing);
    root.append(&form_widget);

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.add_css_class("dialog-actions");
    let save_btn = Button::with_label(tr(&language, "保存修改", "Save Changes"));
    let cancel_btn = Button::with_label(tr(&language, "取消", "Cancel"));
    actions.append(&cancel_btn);
    actions.append(&save_btn);
    root.append(&actions);

    win.set_child(Some(&root));

    let state_for_save = state.clone();
    let form_for_save = form.clone();
    let existing_for_save = existing.clone();
    let win_for_save = win.clone();
    save_btn.connect_clicked(move |_| {
        match collect_edit_submission(&form_for_save, &existing_for_save) {
            Ok(submission) => {
                submit_profile_update(&state_for_save, index, submission, &win_for_save)
            }
            Err(err) => state_for_save.status.set_text(&err.to_string()),
        }
    });

    let win_for_cancel = win.clone();
    cancel_btn.connect_clicked(move |_| {
        win_for_cancel.close();
    });

    win.present();
}

fn open_settings_tab(state: &AppState) {
    close_active_popover(state);
    for index in 0..state.notebook.n_pages() {
        if let Some(page) = state.notebook.nth_page(Some(index))
            && page.has_css_class("settings-page-root")
        {
            activate_session_page(state, &page);
            return;
        }
    }

    let page = build_settings_page(state);
    let language = state.app_settings.borrow().language.clone();
    let title = tr(&language, "设置", "Settings");
    let tab_label = make_tab_label(title, &state.notebook, &page);
    state.notebook.append_page(&page, Some(&tab_label));
    register_page_tab(state, &page, title.to_string());
    activate_session_page(state, page.upcast_ref());
}

fn build_settings_page(state: &AppState) -> ScrolledWindow {
    let language = state.app_settings.borrow().language.clone();
    let root = GtkBox::new(Orientation::Vertical, 10);
    root.add_css_class("settings-page");
    root.set_halign(gtk4::Align::Center);
    root.add_css_class("settings-page-content");

    let title = Label::new(Some(tr(&language, "设置", "Settings")));
    title.set_xalign(0.0);
    title.add_css_class("settings-page-title");
    let subtitle = Label::new(Some(tr(
        &language,
        "调整显示、内置命令环境和保存的会话数据。",
        "Adjust display, built-in tools, and saved session data.",
    )));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.add_css_class("settings-page-subtitle");

    let font_label = Label::new(Some(tr(&language, "终端字体", "Terminal font")));
    font_label.set_xalign(0.0);
    let font_combo = new_stable_combo_box_text();
    populate_font_presets(&font_combo, &state.app_settings.borrow().terminal_font);
    let font_hint = Label::new(Some(tr(
        &language,
        "字体修改会立即应用到所有已打开终端。",
        "Changes apply to all open terminal tabs immediately.",
    )));
    font_hint.set_xalign(0.0);
    font_hint.add_css_class("dim-label");

    let renderer_label = Label::new(Some(tr(&language, "界面渲染器", "UI renderer")));
    renderer_label.set_xalign(0.0);
    let renderer_combo = new_stable_combo_box_text();
    populate_renderer_backend_presets(
        &renderer_combo,
        state.app_settings.borrow().renderer_backend.clone(),
    );
    let renderer_hint = Label::new(Some(tr(
        &language,
        "Cairo 是 CPU 路径。WebGI/GPU 模式映射到 GTK NGL，需要重启。",
        "Cairo is the CPU path. WebGI/GPU mode maps to GTK NGL and needs restart.",
    )));
    renderer_hint.set_xalign(0.0);
    renderer_hint.set_wrap(true);
    renderer_hint.add_css_class("dim-label");

    let semantic_highlight_check = CheckButton::with_label(tr(
        &language,
        "自动高亮终端重要输出",
        "Auto-highlight important terminal output",
    ));
    semantic_highlight_check.set_active(state.app_settings.borrow().semantic_highlighting);
    let semantic_highlight_hint = Label::new(Some(tr(
        &language,
        "在输出没有 ANSI 颜色时，高亮 URL、IP、路径、日期、命令、资源值和警告。",
        "Highlights URLs, IPs, paths, dates, commands, resource values, and warnings when output has no ANSI color.",
    )));
    semantic_highlight_hint.set_xalign(0.0);
    semantic_highlight_hint.set_wrap(true);
    semantic_highlight_hint.add_css_class("dim-label");

    let language_label = Label::new(Some(tr(&language, "界面语言", "Interface language")));
    language_label.set_xalign(0.0);
    let language_combo = new_stable_combo_box_text();
    populate_language_presets(&language_combo, &language);

    let builtin_tools_settings = state.app_settings.borrow().builtin_tools.clone();
    let builtin_tools_enable = CheckButton::with_label(tr(
        &language,
        "启用内置命令环境",
        "Enable built-in command environment",
    ));
    builtin_tools_enable.set_active(builtin_tools_settings.enabled);
    let builtin_tools_ssh = CheckButton::with_label(tr(
        &language,
        "SSH 会话优先使用内置 ssh",
        "Prefer built-in ssh for SSH sessions",
    ));
    builtin_tools_ssh.set_active(builtin_tools_settings.use_for_ssh_client);
    let builtin_tools_inject = CheckButton::with_label(tr(
        &language,
        "注入到 CMD / PowerShell / Git Bash",
        "Inject into CMD / PowerShell / Git Bash",
    ));
    builtin_tools_inject.set_active(builtin_tools_settings.inject_into_system_shells);
    let builtin_tools_priority = CheckButton::with_label(tr(
        &language,
        "内置命令优先于系统 PATH",
        "Prefer built-in commands over system PATH",
    ));
    builtin_tools_priority.set_active(
        builtin_tools_settings.path_priority == BuiltinToolsPathPriority::ToolchainFirst,
    );
    let builtin_tools_status = Label::new(Some(&builtin_toolchain_status_text(&language)));
    builtin_tools_status.set_xalign(0.0);
    builtin_tools_status.set_wrap(true);
    builtin_tools_status.add_css_class("dim-label");

    let openssh_compat = state.app_settings.borrow().openssh_compatibility.clone();
    let openssh_expander = Expander::new(Some(tr(
        &language,
        "OpenSSH 协议版本兼容性支持",
        "OpenSSH protocol compatibility",
    )));
    openssh_expander.add_css_class("settings-expander");
    openssh_expander.set_expanded(openssh_compat.enabled);
    let openssh_box = GtkBox::new(Orientation::Vertical, 8);
    openssh_box.set_margin_top(8);
    let openssh_enable =
        CheckButton::with_label(tr(&language, "启用兼容模式", "Enable compatibility mode"));
    openssh_enable.set_active(openssh_compat.enabled);
    let openssh_hint = Label::new(Some(tr(
        &language,
        "针对旧版 OpenSSH、特殊地区发行版和国密等算法做兼容准备。国密选项需要系统 OpenSSH 本身支持；弱算法需单独勾选。",
        "Prepare compatibility for legacy OpenSSH, regional forks, and national-crypto algorithms. National-crypto options require support from the system OpenSSH; weak algorithms must be enabled explicitly.",
    )));
    openssh_hint.set_xalign(0.0);
    openssh_hint.set_wrap(true);
    openssh_hint.add_css_class("dim-label");
    let openssh_rsa_sha1 = CheckButton::with_label(tr(
        &language,
        "OpenSSH 8.8+ 连接旧服务器：RSA/SHA-1",
        "OpenSSH 8.8+ to legacy servers: RSA/SHA-1",
    ));
    openssh_rsa_sha1.set_active(openssh_compat.rsa_sha1 || openssh_compat.legacy_openssh);
    let openssh_dss = CheckButton::with_label(tr(
        &language,
        "OpenSSH 5.x/6.x：DSA 主机密钥",
        "OpenSSH 5.x/6.x: DSA host keys",
    ));
    openssh_dss.set_active(openssh_compat.dss_host_key || openssh_compat.weak_kex);
    let openssh_legacy_kex = CheckButton::with_label(tr(
        &language,
        "OpenSSH 5.x/6.x：旧 KEX 协商",
        "OpenSSH 5.x/6.x: legacy KEX negotiation",
    ));
    openssh_legacy_kex.set_active(openssh_compat.legacy_kex || openssh_compat.weak_kex);
    let openssh_legacy_ciphers = CheckButton::with_label(tr(
        &language,
        "旧设备：CBC 加密与 HMAC-MD5/SHA1",
        "Legacy devices: CBC ciphers and HMAC-MD5/SHA1",
    ));
    openssh_legacy_ciphers
        .set_active(openssh_compat.legacy_ciphers_macs || openssh_compat.weak_kex);
    let openssh_regional = CheckButton::with_label(tr(
        &language,
        "地区特殊版本 / 国密：SM2、SM3、SM4",
        "Regional forks / national crypto: SM2, SM3, SM4",
    ));
    openssh_regional.set_active(openssh_compat.regional_crypto);
    let openssh_options_box = GtkBox::new(Orientation::Vertical, 6);
    openssh_options_box.set_margin_start(18);
    openssh_options_box.set_sensitive(openssh_compat.enabled);
    append_openssh_compat_row(
        &openssh_options_box,
        &openssh_rsa_sha1,
        &language,
        "添加 HostKeyAlgorithms / PubkeyAcceptedKeyTypes = +ssh-rsa，兼容只支持 RSA/SHA-1 的旧服务器。",
        "Adds HostKeyAlgorithms / PubkeyAcceptedKeyTypes = +ssh-rsa for servers that only support RSA/SHA-1.",
    );
    append_openssh_compat_row(
        &openssh_options_box,
        &openssh_dss,
        &language,
        "添加 HostKeyAlgorithms / PubkeyAcceptedKeyTypes = +ssh-dss，仅用于极旧 DSA 主机。",
        "Adds HostKeyAlgorithms / PubkeyAcceptedKeyTypes = +ssh-dss for very old DSA hosts only.",
    );
    append_openssh_compat_row(
        &openssh_options_box,
        &openssh_legacy_kex,
        &language,
        "添加 KexAlgorithms = +diffie-hellman-group14-sha1,+diffie-hellman-group1-sha1。",
        "Adds KexAlgorithms = +diffie-hellman-group14-sha1,+diffie-hellman-group1-sha1.",
    );
    append_openssh_compat_row(
        &openssh_options_box,
        &openssh_legacy_ciphers,
        &language,
        "添加 Ciphers = +aes128-cbc,+3des-cbc；MACs = +hmac-sha1,+hmac-md5。",
        "Adds Ciphers = +aes128-cbc,+3des-cbc and MACs = +hmac-sha1,+hmac-md5.",
    );
    append_openssh_compat_row(
        &openssh_options_box,
        &openssh_regional,
        &language,
        "添加 SM2 KEX/签名、SM4 加密、HMAC-SM3；需要系统 OpenSSH 支持这些算法名称。",
        "Adds SM2 KEX/signatures, SM4 ciphers, and HMAC-SM3; the system OpenSSH must support these algorithm names.",
    );
    openssh_box.append(&openssh_enable);
    openssh_box.append(&openssh_hint);
    openssh_box.append(&openssh_options_box);
    openssh_expander.set_child(Some(&openssh_box));

    let options_box_for_toggle = openssh_options_box.clone();
    let expander_for_toggle = openssh_expander.clone();
    openssh_enable.connect_toggled(move |button| {
        let enabled = button.is_active();
        options_box_for_toggle.set_sensitive(enabled);
        if enabled {
            expander_for_toggle.set_expanded(true);
        }
    });

    let tools_label = Label::new(Some(tr(&language, "已保存会话", "Saved sessions")));
    tools_label.set_xalign(0.0);
    let tools = GtkBox::new(Orientation::Horizontal, 6);
    let import_btn = Button::with_label(tr(&language, "导入", "Import"));
    let export_btn = Button::with_label(tr(&language, "导出", "Export"));
    tools.append(&import_btn);
    tools.append(&export_btn);

    let version_label = Label::new(Some(&format!(
        "{} {APP_VERSION}",
        tr(&language, "版本", "Version")
    )));
    version_label.set_xalign(0.0);
    version_label.add_css_class("dim-label");

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.add_css_class("settings-actions");
    let save_btn = Button::with_label(tr(&language, "应用", "Apply"));
    let close_btn = Button::with_label(tr(&language, "关闭", "Close"));
    actions.append(&close_btn);
    actions.append(&save_btn);

    let display_card = GtkBox::new(Orientation::Vertical, 8);
    display_card.add_css_class("settings-card");
    let display_title = Label::new(Some(tr(&language, "显示与语言", "Display and language")));
    display_title.set_xalign(0.0);
    display_title.add_css_class("settings-card-title");
    display_card.append(&display_title);
    display_card.append(&font_label);
    display_card.append(font_combo.widget());
    display_card.append(&font_hint);
    display_card.append(&renderer_label);
    display_card.append(renderer_combo.widget());
    display_card.append(&renderer_hint);
    display_card.append(&semantic_highlight_check);
    display_card.append(&semantic_highlight_hint);
    display_card.append(&language_label);
    display_card.append(language_combo.widget());

    let builtin_tools_card = GtkBox::new(Orientation::Vertical, 8);
    builtin_tools_card.add_css_class("settings-card");
    let builtin_tools_title = Label::new(Some(tr(
        &language,
        "内置命令环境",
        "Built-in command environment",
    )));
    builtin_tools_title.set_xalign(0.0);
    builtin_tools_title.add_css_class("settings-card-title");
    builtin_tools_card.append(&builtin_tools_title);
    builtin_tools_card.append(&builtin_tools_enable);
    builtin_tools_card.append(&builtin_tools_ssh);
    builtin_tools_card.append(&builtin_tools_inject);
    builtin_tools_card.append(&builtin_tools_priority);
    builtin_tools_card.append(&builtin_tools_status);

    let sessions_card = GtkBox::new(Orientation::Vertical, 8);
    sessions_card.add_css_class("settings-card");
    tools_label.add_css_class("settings-card-title");
    sessions_card.append(&tools_label);
    sessions_card.append(&tools);

    root.append(&title);
    root.append(&subtitle);
    root.append(&display_card);
    root.append(&builtin_tools_card);
    root.append(&openssh_expander);
    root.append(&sessions_card);
    root.append(&version_label);
    root.append(&actions);

    let page = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&root)
        .build();
    page.add_css_class("settings-page-root");

    let state_for_import = state.clone();
    import_btn.connect_clicked(move |_| {
        show_import_profiles_window(&state_for_import);
    });

    let state_for_export = state.clone();
    export_btn.connect_clicked(move |_| {
        show_export_profiles_window(&state_for_export);
    });

    let state_for_save = state.clone();
    save_btn.connect_clicked(move |_| {
        let previous_settings = state_for_save.app_settings.borrow().clone();
        let selected_font = font_combo
            .active_id()
            .map(|value| value.to_string())
            .unwrap_or_else(|| state_for_save.app_settings.borrow().terminal_font.clone());
        let mut settings = state_for_save.app_settings.borrow().clone();
        settings.terminal_font = selected_font;
        settings.renderer_backend = renderer_backend_from_combo_id(renderer_combo.active_id());
        settings.semantic_highlighting = semantic_highlight_check.is_active();
        settings.language = language_from_combo_id(language_combo.active_id());
        settings.builtin_tools = BuiltinToolsSettings {
            enabled: builtin_tools_enable.is_active(),
            inject_into_system_shells: builtin_tools_inject.is_active(),
            use_for_ssh_client: builtin_tools_ssh.is_active(),
            path_priority: if builtin_tools_priority.is_active() {
                BuiltinToolsPathPriority::ToolchainFirst
            } else {
                BuiltinToolsPathPriority::SystemFirst
            },
        };
        settings.openssh_compatibility = OpensshCompatibilitySettings {
            enabled: openssh_enable.is_active(),
            rsa_sha1: openssh_rsa_sha1.is_active(),
            dss_host_key: openssh_dss.is_active(),
            legacy_kex: openssh_legacy_kex.is_active(),
            legacy_ciphers_macs: openssh_legacy_ciphers.is_active(),
            legacy_openssh: openssh_rsa_sha1.is_active(),
            regional_crypto: openssh_regional.is_active(),
            weak_kex: openssh_dss.is_active()
                || openssh_legacy_kex.is_active()
                || openssh_legacy_ciphers.is_active(),
        };
        let renderer_changed = previous_settings.renderer_backend != settings.renderer_backend;

        match persist_app_settings(&state_for_save, settings) {
            Ok(()) => {
                apply_language_to_main_ui(&state_for_save);
                sync_session_tab_strip(&state_for_save, None);
                let language = state_for_save.app_settings.borrow().language.clone();
                if renderer_changed {
                    state_for_save.status.set_text(tr(
                        &language,
                        "设置已更新。渲染器变更需要重启 R-shell 后生效。",
                        "Settings updated. Restart R-shell to apply the renderer change.",
                    ));
                } else {
                    state_for_save
                        .status
                        .set_text(tr(&language, "设置已更新", "Settings updated"));
                }
            }
            Err(err) => state_for_save.status.set_text(&err.to_string()),
        }
    });

    let notebook_for_close = state.notebook.clone();
    let page_for_close = page.clone();
    close_btn.connect_clicked(move |_| {
        if let Some(page_num) = notebook_for_close.page_num(&page_for_close) {
            notebook_for_close.remove_page(Some(page_num));
        }
    });

    page
}

fn append_openssh_compat_row(
    container: &GtkBox,
    check: &CheckButton,
    language: &AppLanguage,
    zh_cn_detail: &'static str,
    en_us_detail: &'static str,
) {
    let detail = Label::new(Some(tr(language, zh_cn_detail, en_us_detail)));
    detail.set_xalign(0.0);
    detail.set_wrap(true);
    detail.set_margin_start(26);
    detail.add_css_class("dim-label");

    container.append(check);
    container.append(&detail);
}

fn builtin_toolchain_status_text(language: &AppLanguage) -> String {
    let discovery = BuiltinToolchain::discover();
    if let Some(toolchain) = discovery.toolchain {
        return format!(
            "{}: {}",
            tr(language, "已找到", "Found"),
            toolchain.root.display()
        );
    }

    if !discovery.missing_commands.is_empty() {
        return format!(
            "{}: {}",
            tr(language, "缺少命令", "Missing commands"),
            discovery.missing_commands.join(", ")
        );
    }

    tr(language, "未找到内置工具链", "Built-in toolchain not found").to_string()
}

fn new_stable_combo_box_text() -> StableComboBox {
    StableComboBox::new()
}

impl StableComboBox {
    fn new() -> Self {
        let button = Button::new();
        button.add_css_class("stable-dropdown");
        button.set_hexpand(true);
        button.set_halign(gtk4::Align::Fill);
        button.set_size_request(-1, 36);

        let row = GtkBox::new(Orientation::Horizontal, 8);
        row.set_hexpand(true);
        let value_label = Label::new(None);
        value_label.set_xalign(0.0);
        value_label.set_hexpand(true);
        let arrow = Image::from_icon_name("pan-down-symbolic");
        arrow.set_pixel_size(14);
        row.append(&value_label);
        row.append(&arrow);
        button.set_child(Some(&row));

        let combo = Self {
            button,
            value_label,
            options: Rc::new(RefCell::new(Vec::new())),
            active_id: Rc::new(RefCell::new(None)),
            changed_handlers: Rc::new(RefCell::new(Vec::new())),
        };
        combo.connect_popup();
        combo
    }

    fn widget(&self) -> &Button {
        &self.button
    }

    fn append(&self, id: Option<&str>, label: &str) {
        let id = id.unwrap_or(label).to_string();
        self.options.borrow_mut().push(StableComboOption {
            id,
            label: label.to_string(),
        });
    }

    fn active_id(&self) -> Option<String> {
        self.active_id.borrow().clone()
    }

    fn set_active_id(&self, id: Option<&str>) -> bool {
        let Some(id) = id else {
            return false;
        };
        let option = self
            .options
            .borrow()
            .iter()
            .find(|option| option.id == id)
            .cloned();
        let Some(option) = option else {
            return false;
        };

        let changed = self.active_id.borrow().as_deref() != Some(option.id.as_str());
        *self.active_id.borrow_mut() = Some(option.id);
        self.value_label.set_text(&option.label);
        if changed {
            for handler in self.changed_handlers.borrow().iter() {
                handler(self);
            }
        }
        true
    }

    fn connect_changed(&self, handler: impl Fn(&StableComboBox) + 'static) {
        self.changed_handlers.borrow_mut().push(Box::new(handler));
    }

    fn connect_popup(&self) {
        let combo = self.clone();
        self.button.connect_clicked(move |button| {
            let popover = Popover::new();
            popover.set_has_arrow(false);
            popover.set_autohide(true);
            popover.set_position(PositionType::Bottom);
            popover.set_offset(0, 4);
            popover.set_parent(button);
            popover.add_css_class("app-menu-popover");

            let root = GtkBox::new(Orientation::Vertical, 2);
            root.add_css_class("context-menu-list");
            for option in combo.options.borrow().iter().cloned() {
                let option_row = GtkBox::new(Orientation::Horizontal, 0);
                option_row.add_css_class("stable-dropdown-option");
                option_row.set_can_focus(true);
                option_row.set_focusable(true);

                let option_label = Label::new(Some(&option.label));
                option_label.set_xalign(0.5);
                option_label.set_hexpand(true);
                option_row.append(&option_label);

                let motion = EventControllerMotion::new();
                let row_for_enter = option_row.clone();
                motion.connect_enter(move |_, _, _| {
                    row_for_enter.add_css_class("hover");
                });
                let row_for_leave = option_row.clone();
                motion.connect_leave(move |_| {
                    row_for_leave.remove_css_class("hover");
                });
                option_row.add_controller(motion);

                let click = GestureClick::new();
                click.set_button(1);
                let combo_for_option = combo.clone();
                let popover_for_option = popover.clone();
                click.connect_released(move |_, _, _, _| {
                    combo_for_option.set_active_id(Some(&option.id));
                    popover_for_option.popdown();
                });
                option_row.add_controller(click);

                root.append(&option_row);
            }
            popover.set_child(Some(&root));
            popover.popup();
        });
    }
}

fn populate_font_presets(combo: &StableComboBox, current_font: &str) {
    for (label, font_description) in TERMINAL_FONT_PRESETS {
        combo.append(Some(font_description), label);
    }
    if !TERMINAL_FONT_PRESETS
        .iter()
        .any(|(_, font_description)| *font_description == current_font)
    {
        combo.append(Some(current_font), &format!("Current ({current_font})"));
    }
    combo.set_active_id(Some(current_font));
}

fn populate_renderer_backend_presets(combo: &StableComboBox, current_backend: RendererBackend) {
    combo.append(Some("cairo"), "CPU / Cairo");
    combo.append(Some("ngl"), "WebGI / GPU (GTK NGL, experimental)");
    combo.set_active_id(Some(match current_backend {
        RendererBackend::Cairo => "cairo",
        RendererBackend::Ngl => "ngl",
    }));
}

fn renderer_backend_from_combo_id(value: Option<String>) -> RendererBackend {
    match value.as_deref() {
        Some("ngl") => RendererBackend::Ngl,
        _ => RendererBackend::Cairo,
    }
}

fn populate_language_presets(combo: &StableComboBox, current_language: &AppLanguage) {
    combo.append(Some("zh-cn"), "中文");
    combo.append(Some("en-us"), "English");
    combo.set_active_id(Some(match current_language {
        AppLanguage::ZhCn => "zh-cn",
        AppLanguage::EnUs => "en-us",
    }));
}

fn language_from_combo_id(value: Option<String>) -> AppLanguage {
    match value.as_deref() {
        Some("en-us") => AppLanguage::EnUs,
        _ => AppLanguage::ZhCn,
    }
}

fn apply_language_to_main_ui(state: &AppState) {
    let language = state.app_settings.borrow().language.clone();
    state
        .chrome
        .new_session_button
        .set_label(tr(&language, "新建", "New"));
    state
        .chrome
        .settings_button
        .set_label(tr(&language, "设置", "Settings"));
    state
        .chrome
        .sidebar_title
        .set_text(tr(&language, "已保存会话", "Saved Sessions"));
    state.sidebar_nav.sessions_button.set_tooltip_text(Some(tr(
        &language,
        "已保存会话",
        "Sessions",
    )));
    state.sidebar_nav.sftp_button.set_tooltip_text(Some(tr(
        &language,
        "SFTP 浏览器",
        "SFTP browser",
    )));
    state.session_tab_strip.add_button.set_tooltip_text(Some(tr(
        &language,
        "打开首选本地终端",
        "Open the preferred local terminal",
    )));
    state
        .session_tab_strip
        .dropdown_button
        .set_tooltip_text(Some(tr(
            &language,
            "选择本地终端",
            "Choose a local terminal",
        )));
    state
        .sftp_sidebar
        .title_label
        .set_text(tr(&language, "SFTP 浏览器", "SFTP Browser"));
    state
        .sftp_sidebar
        .search_entry
        .set_placeholder_text(Some(tr(
            &language,
            "搜索当前目录文件",
            "Search files in current directory",
        )));
    state
        .sftp_sidebar
        .refresh_button
        .set_tooltip_text(Some(tr(&language, "刷新", "Refresh")));
    state.sftp_sidebar.upload_button.set_tooltip_text(Some(tr(
        &language,
        "使用系统文件选择器上传",
        "Upload using the system file picker",
    )));
    if state.sftp_sidebar.session.borrow().is_none() {
        state.sftp_sidebar.path_label.set_text(tr(
            &language,
            "选择一个 SSH 或 SFTP 会话。",
            "Select an SSH or SFTP session.",
        ));
        state.sftp_sidebar.status_label.set_text(tr(
            &language,
            "点击 SFTP 图标浏览当前远程会话。",
            "Click the SFTP icon to browse the current remote session.",
        ));
    }
    for index in 0..state.notebook.n_pages() {
        if let Some(page) = state.notebook.nth_page(Some(index))
            && page.has_css_class("settings-page-root")
        {
            register_page_tab(state, &page, tr(&language, "设置", "Settings"));
        }
    }
}

fn persist_app_settings(state: &AppState, settings: AppSettings) -> anyhow::Result<()> {
    state
        .profile_store
        .save_settings(&settings)
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    *state.app_settings.borrow_mut() = settings.clone();
    state
        .terminal_appearance
        .set_font_description(settings.terminal_font);
    state
        .terminal_appearance
        .set_semantic_highlighting(settings.semantic_highlighting);
    refresh_terminal_widgets(state);
    Ok(())
}

fn register_terminal_widget(state: &AppState, widget: &DrawingArea) {
    let weak = glib::WeakRef::<DrawingArea>::new();
    weak.set(Some(widget));
    state.terminal_widgets.borrow_mut().push(weak);
}

fn install_terminal_font_zoom_handler(state: &AppState, view: &GtkTerminalView) {
    let state_for_zoom = state.clone();
    view.set_font_zoom_handler(move |steps| {
        zoom_terminal_font(&state_for_zoom, steps);
    });
}

fn zoom_terminal_font(state: &AppState, steps: i32) {
    let current_font = state.app_settings.borrow().terminal_font.clone();
    let next_font = adjusted_font_description_size(&current_font, steps);
    if next_font == current_font {
        return;
    }

    let mut settings = state.app_settings.borrow().clone();
    settings.terminal_font = next_font.clone();
    match persist_app_settings(state, settings) {
        Ok(()) => {
            let language = state.app_settings.borrow().language.clone();
            let size = next_font.split_whitespace().last().unwrap_or(&next_font);
            state.status.set_text(&format!(
                "{}: {} pt",
                tr(&language, "终端字体大小", "Terminal font size"),
                size
            ));
        }
        Err(err) => state.status.set_text(&err.to_string()),
    }
}

fn register_page_context(
    state: &AppState,
    widget: &impl gtk4::prelude::IsA<Widget>,
    profile: ConnectionProfile,
    runtime_password: Option<String>,
) {
    prune_page_contexts(state);

    let weak = glib::WeakRef::<Widget>::new();
    weak.set(Some(widget.upcast_ref()));
    state.page_contexts.borrow_mut().push(PageContext {
        widget: weak,
        profile,
        runtime_password,
    });
}

fn prune_page_contexts(state: &AppState) {
    state.page_contexts.borrow_mut().retain(|context| {
        context
            .widget
            .upgrade()
            .is_some_and(|widget| state.notebook.page_num(&widget).is_some())
    });
}

fn register_page_shutdown(
    state: &AppState,
    widget: &impl gtk4::prelude::IsA<Widget>,
    action: impl Fn() + 'static,
) {
    let weak = glib::WeakRef::<Widget>::new();
    weak.set(Some(widget.upcast_ref()));

    let mut shutdowns = state.page_shutdowns.borrow_mut();
    shutdowns.retain(|shutdown| {
        shutdown
            .widget
            .upgrade()
            .is_some_and(|page| state.notebook.page_num(&page).is_some())
    });
    shutdowns.push(PageShutdown {
        widget: weak,
        action: Rc::new(action),
    });
}

fn invoke_page_shutdown(state: &AppState, page: &Widget) {
    let page_ptr = page.as_ptr();
    let mut action = None;
    let mut shutdowns = state.page_shutdowns.borrow_mut();
    let mut retained = Vec::with_capacity(shutdowns.len());

    for shutdown in shutdowns.drain(..) {
        let Some(widget) = shutdown.widget.upgrade() else {
            continue;
        };
        if widget.as_ptr() == page_ptr {
            action = Some(shutdown.action);
            continue;
        }
        if state.notebook.page_num(&widget).is_some() {
            retained.push(shutdown);
        }
    }

    *shutdowns = retained;
    drop(shutdowns);

    if let Some(action) = action {
        action();
    }
}

fn prune_session_handles(state: &AppState) {
    let mut connections = state.connections.borrow_mut();
    connections
        .locals
        .retain(|connection| Rc::strong_count(connection) > 1);
    connections
        .ssh
        .retain(|connection| Rc::strong_count(connection) > 1);
    connections
        .telnet
        .retain(|connection| Rc::strong_count(connection) > 1);
    connections
        .serial
        .retain(|connection| Rc::strong_count(connection) > 1);
    connections
        .ftp
        .retain(|connection| Rc::strong_count(connection) > 1);
}

fn schedule_session_handle_prune(state: &AppState) {
    prune_session_handles(state);

    for delay in [75_u64, 250, 1_000] {
        let state_for_prune = state.clone();
        glib::timeout_add_local(Duration::from_millis(delay), move || {
            prune_session_handles(&state_for_prune);
            ControlFlow::Break
        });
    }
}

#[cfg(windows)]
fn schedule_post_close_memory_trim() {
    for delay in [250_u64, 1_000, 3_000] {
        glib::timeout_add_local(Duration::from_millis(delay), || {
            trim_process_working_set();
            ControlFlow::Break
        });
    }
}

#[cfg(not(windows))]
fn schedule_post_close_memory_trim() {}

fn current_page_context(state: &AppState) -> Option<PageContext> {
    let current_page = state
        .notebook
        .current_page()
        .and_then(|index| state.notebook.nth_page(Some(index)))?;

    page_context_for_widget(state, &current_page)
}

fn page_context_for_widget(state: &AppState, page: &Widget) -> Option<PageContext> {
    prune_page_contexts(state);
    let page_ptr = page.as_ptr();

    state.page_contexts.borrow().iter().find_map(|context| {
        let widget = context.widget.upgrade()?;
        (widget.as_ptr() == page_ptr).then_some(context.clone())
    })
}

fn sync_active_page_state(state: &AppState, context: Option<PageContext>) {
    sync_host_stats_for_page_context(state, context.as_ref());
    if state.sidebar_nav.stack.visible_child_name().as_deref() == Some("sftp") {
        activate_sftp_sidebar_for_page_context(state, context);
    }
}

fn sync_host_stats_for_page_context(state: &AppState, context: Option<&PageContext>) {
    let Some(context) = context else {
        reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
        return;
    };

    if !matches!(
        context.profile.protocol,
        ProtocolKind::Ssh | ProtocolKind::Sftp
    ) {
        reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
        return;
    }

    let Some(host) = context.profile.host.clone() else {
        reset_host_stats(
            state,
            "Host: unavailable    CPU: -    Mem: -    Net: -    Uptime: -",
        );
        return;
    };

    request_host_stats(
        state,
        host,
        context.profile.port.unwrap_or(22),
        context.profile.auth.username().map(str::to_owned),
        context.runtime_password.clone(),
    );
}

fn refresh_terminal_widgets(state: &AppState) {
    let mut widgets = state.terminal_widgets.borrow_mut();
    widgets.retain(|weak| {
        if let Some(widget) = weak.upgrade() {
            widget.queue_draw();
            true
        } else {
            false
        }
    });
}

fn show_export_profiles_window(state: &AppState) {
    let state_for_export = state.clone();
    show_path_prompt(
        &state.window,
        "Export Saved Sessions",
        "Creates an encrypted backup of all saved sessions, including passwords, for this Windows account.",
        "Export",
        default_profiles_export_path(),
        move |path| {
            let store = state_for_export.profile_store.clone();
            let document = state_for_export.profiles_doc.borrow().clone();
            let export_path = path.clone();
            run_background_task(
                &state_for_export,
                Some("Exporting saved sessions..."),
                move || {
                    store
                        .export_bundle(&document, &export_path)
                        .map_err(|err| anyhow::anyhow!(err.to_string()))
                },
                move |state, result| match result {
                    Ok(()) => state
                        .status
                        .set_text(&format!("Saved sessions exported to {}", path.display())),
                    Err(err) => state.status.set_text(&format!("Export failed: {err}")),
                },
            );
        },
    );
}

fn show_import_profiles_window(state: &AppState) {
    let state_for_import = state.clone();
    show_path_prompt(
        &state.window,
        "Import Saved Sessions",
        "Replaces the current saved sessions with an encrypted backup created for this Windows account.",
        "Import",
        default_profiles_export_path(),
        move |path| {
            let store = state_for_import.profile_store.clone();
            let import_path = path.clone();
            run_background_task(
                &state_for_import,
                Some("Importing saved sessions..."),
                move || {
                    store
                        .import_bundle(&import_path)
                        .map_err(|err| anyhow::anyhow!(err.to_string()))
                },
                move |state, result| match result {
                    Ok(document) => {
                        *state.profiles_doc.borrow_mut() = document;
                        refresh_profile_list_for_state(state);
                        state
                            .status
                            .set_text(&format!("Saved sessions imported from {}", path.display()));
                    }
                    Err(err) => state.status.set_text(&format!("Import failed: {err}")),
                },
            );
        },
    );
}

fn show_path_prompt(
    parent: &ApplicationWindow,
    title: &str,
    message: &str,
    confirm_label: &str,
    default_path: PathBuf,
    on_submit: impl Fn(PathBuf) + 'static,
) {
    let win = build_modal_window(parent, title, 520, 0);

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let message_label = Label::new(Some(message));
    message_label.set_xalign(0.0);
    message_label.set_wrap(true);
    let path_entry = Entry::builder().hexpand(true).build();
    path_entry.set_text(&default_path.display().to_string());

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    let confirm_btn = Button::with_label(confirm_label);
    let cancel_btn = Button::with_label("Cancel");
    actions.append(&confirm_btn);
    actions.append(&cancel_btn);

    root.append(&message_label);
    root.append(&path_entry);
    root.append(&actions);
    win.set_child(Some(&root));

    let win_for_confirm = win.clone();
    confirm_btn.connect_clicked(move |_| {
        let path = PathBuf::from(path_entry.text().trim());
        if path.as_os_str().is_empty() {
            return;
        }
        win_for_confirm.close();
        on_submit(path);
    });

    let win_for_cancel = win.clone();
    cancel_btn.connect_clicked(move |_| {
        win_for_cancel.close();
    });

    win.present();
}

fn show_open_files_dialog(
    parent: &ApplicationWindow,
    title: &str,
    accept_label: &str,
    on_submit: impl Fn(Vec<PathBuf>) + 'static,
) {
    let dialog = FileChooserNative::new(
        Some(title),
        Some(parent),
        FileChooserAction::Open,
        Some(accept_label),
        Some("Cancel"),
    );
    dialog.set_select_multiple(true);
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept {
            let files = dialog.files();
            let mut paths = Vec::new();
            for index in 0..files.n_items() {
                if let Some(file) = files
                    .item(index)
                    .and_then(|item| item.downcast::<gio::File>().ok())
                    .and_then(|file| file.path())
                {
                    paths.push(file);
                }
            }
            if paths.is_empty()
                && let Some(path) = dialog.file().and_then(|file| file.path())
            {
                paths.push(path);
            }
            if !paths.is_empty() {
                on_submit(paths);
            }
        }
        dialog.destroy();
    });
    dialog.show();
}

fn show_save_file_dialog(
    parent: &ApplicationWindow,
    title: &str,
    suggested_name: &str,
    on_submit: impl Fn(PathBuf) + 'static,
) {
    let dialog = FileChooserNative::new(
        Some(title),
        Some(parent),
        FileChooserAction::Save,
        Some("Save"),
        Some("Cancel"),
    );
    dialog.set_current_name(suggested_name);
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
        {
            on_submit(path);
        }
        dialog.destroy();
    });
    dialog.show();
}

fn show_select_folder_dialog(
    parent: &ApplicationWindow,
    title: &str,
    accept_label: &str,
    on_submit: impl Fn(PathBuf) + 'static,
) {
    let dialog = FileChooserNative::new(
        Some(title),
        Some(parent),
        FileChooserAction::SelectFolder,
        Some(accept_label),
        Some("Cancel"),
    );
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept
            && let Some(path) = dialog.file().and_then(|file| file.path())
        {
            on_submit(path);
        }
        dialog.destroy();
    });
    dialog.show();
}

fn show_text_prompt(
    parent: &impl gtk4::prelude::IsA<Window>,
    title: &str,
    label_text: &str,
    initial_text: &str,
    confirm_label: &str,
    cancel_label: &str,
    on_submit: impl Fn(String) + 'static,
) {
    let win = build_modal_window(parent, title, 360, 0);

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let label = Label::new(Some(label_text));
    label.set_xalign(0.0);
    let entry = Entry::builder()
        .placeholder_text(label_text)
        .hexpand(true)
        .build();
    if !initial_text.is_empty() {
        entry.set_text(initial_text);
    }

    let actions = GtkBox::new(Orientation::Horizontal, 6);
    let confirm_btn = Button::with_label(confirm_label);
    let cancel_btn = Button::with_label(cancel_label);
    actions.append(&confirm_btn);
    actions.append(&cancel_btn);

    root.append(&label);
    root.append(&entry);
    root.append(&actions);
    win.set_child(Some(&root));

    let win_for_confirm = win.clone();
    confirm_btn.connect_clicked(move |_| {
        let value = entry.text().trim().to_string();
        win_for_confirm.close();
        if !value.is_empty() {
            on_submit(value);
        }
    });

    let win_for_cancel = win.clone();
    cancel_btn.connect_clicked(move |_| {
        win_for_cancel.close();
    });

    win.present();
}

fn populate_form_from_profile(form: &NewSessionForm, profile: &ConnectionProfile) {
    match profile.protocol {
        ProtocolKind::LocalShell => {}
        ProtocolKind::Ssh => {
            form.protocol_stack.set_visible_child_name("ssh");
            form.ssh_name.set_text(&profile.name);
            form.ssh_host
                .set_text(profile.host.as_deref().unwrap_or_default());
            form.ssh_port
                .set_text(&profile.port.unwrap_or(22).to_string());
            populate_auth_fields(&form.ssh_auth, &profile.auth);
        }
        ProtocolKind::Sftp => {
            form.protocol_stack.set_visible_child_name("sftp");
            form.sftp_name.set_text(&profile.name);
            form.sftp_host
                .set_text(profile.host.as_deref().unwrap_or_default());
            form.sftp_port
                .set_text(&profile.port.unwrap_or(22).to_string());
            populate_auth_fields(&form.sftp_auth, &profile.auth);
        }
        ProtocolKind::Ftp => {
            form.protocol_stack.set_visible_child_name("ftp");
            form.ftp_name.set_text(&profile.name);
            form.ftp_host
                .set_text(profile.host.as_deref().unwrap_or_default());
            form.ftp_port
                .set_text(&profile.port.unwrap_or(21).to_string());
            form.ftp_username
                .set_text(profile.auth.username().unwrap_or_default());
            form.ftp_remember.set_active(matches!(
                profile.auth,
                AuthConfig::Password {
                    password_ref: CredentialsRef::SystemKeychain { .. },
                    ..
                }
            ));
            form.ftp_password
                .set_placeholder_text(Some("Leave blank to keep saved password"));
        }
        ProtocolKind::Telnet => {
            form.protocol_stack.set_visible_child_name("telnet");
            form.telnet_name.set_text(&profile.name);
            form.telnet_host
                .set_text(profile.host.as_deref().unwrap_or_default());
            form.telnet_port
                .set_text(&profile.port.unwrap_or(23).to_string());
        }
        ProtocolKind::Serial => {
            form.protocol_stack.set_visible_child_name("serial");
            form.serial_name.set_text(&profile.name);
            form.serial_port
                .set_text(profile.serial_port.as_deref().unwrap_or_default());
            form.serial_baud
                .set_text(&profile.baud_rate.unwrap_or(115_200).to_string());
        }
        ProtocolKind::Vnc => {}
    }
}

fn populate_auth_fields(fields: &AuthFields, auth: &AuthConfig) {
    match auth {
        AuthConfig::None => {}
        AuthConfig::KeyboardInteractive { username } => {
            fields.mode.set_active_id(Some("keyboard"));
            fields.stack.set_visible_child_name("keyboard");
            fields.keyboard_username.set_text(username);
        }
        AuthConfig::Password {
            username,
            password_ref,
        } => {
            fields.mode.set_active_id(Some("password"));
            fields.stack.set_visible_child_name("password");
            fields.password.username.set_text(username);
            fields.password.remember.set_active(matches!(
                password_ref,
                CredentialsRef::SystemKeychain { .. }
            ));
            fields
                .password
                .password
                .set_placeholder_text(Some("Leave blank to keep saved password"));
        }
        AuthConfig::PrivateKey { username, key_ref } => {
            fields.mode.set_active_id(Some("private-key"));
            fields.stack.set_visible_child_name("private-key");
            fields.private_key.username.set_text(username);
            if let CredentialsRef::PrivateKey { path, .. } = key_ref {
                fields.private_key.key_path.set_text(path);
            }
        }
    }
}

fn collect_edit_submission(
    form: &NewSessionForm,
    existing: &ConnectionProfile,
) -> anyhow::Result<ProfileSubmission> {
    let mut submission = form.collect()?;
    submission.profile.id = existing.id;

    if let AuthConfig::Password {
        username,
        password_ref,
    } = &mut submission.profile.auth
        && matches!(password_ref, CredentialsRef::SystemKeychain { .. })
    {
        *password_ref = CredentialsRef::SystemKeychain {
            service: credential_service(existing),
            account: username.clone(),
        };
        if let Some(pending_secret) = submission.pending_secret.as_mut() {
            pending_secret.reference = password_ref.clone();
        }
    }

    if submission.runtime_password.is_none()
        && submission.profile.protocol == existing.protocol
        && let (
            AuthConfig::Password {
                username,
                password_ref,
            },
            AuthConfig::Password {
                username: old_username,
                password_ref: old_password_ref,
            },
        ) = (&mut submission.profile.auth, &existing.auth)
        && username == old_username
        && matches!(old_password_ref, CredentialsRef::SystemKeychain { .. })
    {
        *password_ref = old_password_ref.clone();
        submission.pending_secret = None;
    }

    Ok(submission)
}

fn build_new_session_form(language: &AppLanguage) -> (GtkBox, NewSessionForm) {
    let container = GtkBox::new(Orientation::Vertical, 8);
    container.add_css_class("new-session-form");
    let switcher = StackSwitcher::new();
    switcher.add_css_class("protocol-switcher");
    let protocol_stack = Stack::new();
    protocol_stack.set_transition_type(StackTransitionType::Crossfade);
    protocol_stack.set_transition_duration(SIDEBAR_STACK_TRANSITION_MS);
    protocol_stack.set_vexpand(true);
    protocol_stack.add_css_class("protocol-stack");
    switcher.set_stack(Some(&protocol_stack));

    let ssh_name = new_entry(tr(language, "SSH 会话名称", "SSH profile name"), None);
    let ssh_host = new_entry("192.168.6.111", None);
    let ssh_port = new_entry("22", Some("22"));
    let ssh_auth = build_auth_fields(true, language);
    let ssh_grid = form_grid();
    add_form_row(&ssh_grid, 0, tr(language, "名称", "Name"), &ssh_name);
    add_form_row(&ssh_grid, 1, tr(language, "主机", "Host"), &ssh_host);
    add_form_row(&ssh_grid, 2, tr(language, "端口", "Port"), &ssh_port);
    add_form_row(
        &ssh_grid,
        3,
        tr(language, "认证", "Auth"),
        ssh_auth.mode.widget(),
    );
    add_form_row(
        &ssh_grid,
        4,
        tr(language, "详情", "Details"),
        &ssh_auth.stack,
    );
    protocol_stack.add_titled(&ssh_grid, Some("ssh"), "SSH");

    let sftp_name = new_entry(tr(language, "SFTP 会话名称", "SFTP profile name"), None);
    let sftp_host = new_entry("192.168.6.111", None);
    let sftp_port = new_entry("22", Some("22"));
    let sftp_auth = build_auth_fields(false, language);
    let sftp_grid = form_grid();
    add_form_row(&sftp_grid, 0, tr(language, "名称", "Name"), &sftp_name);
    add_form_row(&sftp_grid, 1, tr(language, "主机", "Host"), &sftp_host);
    add_form_row(&sftp_grid, 2, tr(language, "端口", "Port"), &sftp_port);
    add_form_row(
        &sftp_grid,
        3,
        tr(language, "认证", "Auth"),
        sftp_auth.mode.widget(),
    );
    add_form_row(
        &sftp_grid,
        4,
        tr(language, "详情", "Details"),
        &sftp_auth.stack,
    );
    protocol_stack.add_titled(&sftp_grid, Some("sftp"), "SFTP");

    let ftp_name = new_entry(tr(language, "FTP 会话名称", "FTP profile name"), None);
    let ftp_host = new_entry("192.168.6.111", None);
    let ftp_port = new_entry("21", Some("21"));
    let ftp_username = new_entry("root", None);
    let ftp_password = new_password_entry(tr(language, "密码", "Password"));
    let ftp_remember = CheckButton::with_label(tr(
        language,
        "在本机安全保存密码",
        "Save password securely on this PC",
    ));
    let ftp_password_box = GtkBox::new(Orientation::Vertical, 6);
    ftp_password_box.append(&ftp_password);
    ftp_password_box.append(&ftp_remember);
    let ftp_grid = form_grid();
    add_form_row(&ftp_grid, 0, tr(language, "名称", "Name"), &ftp_name);
    add_form_row(&ftp_grid, 1, tr(language, "主机", "Host"), &ftp_host);
    add_form_row(&ftp_grid, 2, tr(language, "端口", "Port"), &ftp_port);
    add_form_row(
        &ftp_grid,
        3,
        tr(language, "用户名", "Username"),
        &ftp_username,
    );
    add_form_row(
        &ftp_grid,
        4,
        tr(language, "密码", "Password"),
        &ftp_password_box,
    );
    protocol_stack.add_titled(&ftp_grid, Some("ftp"), "FTP");

    let telnet_name = new_entry(tr(language, "Telnet 会话名称", "Telnet profile name"), None);
    let telnet_host = new_entry("192.168.6.111", None);
    let telnet_port = new_entry("23", Some("23"));
    let telnet_grid = form_grid();
    add_form_row(&telnet_grid, 0, tr(language, "名称", "Name"), &telnet_name);
    add_form_row(&telnet_grid, 1, tr(language, "主机", "Host"), &telnet_host);
    add_form_row(&telnet_grid, 2, tr(language, "端口", "Port"), &telnet_port);
    protocol_stack.add_titled(&telnet_grid, Some("telnet"), "Telnet");

    let serial_name = new_entry(tr(language, "串口会话名称", "Serial profile name"), None);
    let serial_port = new_entry("COM3 or /dev/ttyUSB0", None);
    let serial_baud = new_entry("115200", Some("115200"));
    let serial_grid = form_grid();
    add_form_row(&serial_grid, 0, tr(language, "名称", "Name"), &serial_name);
    add_form_row(&serial_grid, 1, tr(language, "端口", "Port"), &serial_port);
    add_form_row(
        &serial_grid,
        2,
        tr(language, "波特率", "Baud"),
        &serial_baud,
    );
    protocol_stack.add_titled(&serial_grid, Some("serial"), "Serial");

    protocol_stack.set_visible_child_name("ssh");

    container.append(&switcher);
    container.append(&protocol_stack);

    (
        container,
        NewSessionForm {
            protocol_stack,
            ssh_name,
            ssh_host,
            ssh_port,
            ssh_auth,
            sftp_name,
            sftp_host,
            sftp_port,
            sftp_auth,
            ftp_name,
            ftp_host,
            ftp_port,
            ftp_username,
            ftp_password,
            ftp_remember,
            telnet_name,
            telnet_host,
            telnet_port,
            serial_name,
            serial_port,
            serial_baud,
        },
    )
}

fn build_auth_fields(allow_keyboard: bool, language: &AppLanguage) -> AuthFields {
    let mode = new_stable_combo_box_text();
    if allow_keyboard {
        mode.append(Some("keyboard"), tr(language, "键盘交互", "Keyboard input"));
    }
    mode.append(Some("password"), tr(language, "密码", "Password"));
    mode.append(Some("private-key"), tr(language, "私钥", "Private key"));

    let stack = Stack::new();
    stack.set_transition_type(StackTransitionType::Crossfade);
    stack.set_transition_duration(PAGE_SWITCH_TRANSITION_MS as u32);
    stack.add_css_class("auth-stack");

    let keyboard_username = new_entry("root", None);
    let keyboard_grid = form_grid();
    add_form_row(
        &keyboard_grid,
        0,
        tr(language, "用户名", "Username"),
        &keyboard_username,
    );
    let keyboard_hint = Label::new(Some(tr(
        language,
        "认证提示会保留在终端页签中交互。",
        "Prompts stay interactive in the terminal tab.",
    )));
    keyboard_hint.set_xalign(0.0);
    keyboard_hint.set_wrap(true);
    keyboard_hint.add_css_class("dim-label");
    keyboard_grid.attach(&keyboard_hint, 1, 1, 1, 1);
    if allow_keyboard {
        stack.add_titled(
            &keyboard_grid,
            Some("keyboard"),
            tr(language, "键盘交互", "Keyboard input"),
        );
    }

    let password = PasswordFields {
        username: new_entry("root", None),
        password: new_password_entry(tr(language, "密码", "Password")),
        remember: CheckButton::with_label(tr(
            language,
            "在本机安全保存密码",
            "Save password securely on this PC",
        )),
    };
    let password_grid = form_grid();
    add_form_row(
        &password_grid,
        0,
        tr(language, "用户名", "Username"),
        &password.username,
    );
    add_form_row(
        &password_grid,
        1,
        tr(language, "密码", "Password"),
        &password.password,
    );
    password_grid.attach(&password.remember, 1, 2, 1, 1);
    stack.add_titled(
        &password_grid,
        Some("password"),
        tr(language, "密码", "Password"),
    );

    let private_key = KeyFields {
        username: new_entry("root", None),
        key_path: new_entry("C:/keys/id_ed25519", None),
    };
    let key_grid = form_grid();
    add_form_row(
        &key_grid,
        0,
        tr(language, "用户名", "Username"),
        &private_key.username,
    );
    add_form_row(
        &key_grid,
        1,
        tr(language, "私钥", "Private key"),
        &private_key.key_path,
    );
    stack.add_titled(
        &key_grid,
        Some("private-key"),
        tr(language, "私钥", "Private key"),
    );

    let stack_for_mode = stack.clone();
    mode.connect_changed(move |combo| {
        if let Some(active_id) = combo.active_id() {
            stack_for_mode.set_visible_child_name(active_id.as_str());
        }
    });

    mode.set_active_id(Some(if allow_keyboard {
        "keyboard"
    } else {
        "password"
    }));
    stack.set_visible_child_name(if allow_keyboard {
        "keyboard"
    } else {
        "password"
    });

    AuthFields {
        mode,
        stack,
        keyboard_username,
        password,
        private_key,
    }
}

fn form_grid() -> Grid {
    let grid = Grid::new();
    grid.set_column_spacing(12);
    grid.set_row_spacing(8);
    grid.set_margin_top(8);
    grid.set_margin_bottom(8);
    grid.set_margin_start(8);
    grid.set_margin_end(8);
    grid.add_css_class("form-grid");
    grid.add_css_class("protocol-page");
    grid
}

fn add_form_row(grid: &Grid, row: i32, label_text: &str, widget: &impl gtk4::prelude::IsA<Widget>) {
    let label = Label::new(Some(label_text));
    label.set_xalign(0.0);
    label.set_halign(gtk4::Align::Start);
    label.add_css_class("form-label");
    widget.as_ref().set_hexpand(true);
    grid.attach(&label, 0, row, 1, 1);
    grid.attach(widget, 1, row, 1, 1);
}

fn new_entry(placeholder: &str, initial: Option<&str>) -> Entry {
    let entry = Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build();
    if let Some(initial) = initial {
        entry.set_text(initial);
    }
    entry
}

fn new_password_entry(placeholder: &str) -> Entry {
    let entry = new_entry(placeholder, None);
    entry.set_visibility(false);
    entry
}

fn default_profile_name(entry: &Entry, fallback: &str) -> String {
    let text = entry.text();
    let value = text.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn default_entry_text(entry: &Entry, fallback: &str) -> String {
    let text = entry.text();
    let value = text.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

fn require_entry_text(label: &str, entry: &Entry) -> anyhow::Result<String> {
    let value = entry.text().trim().to_string();
    if value.is_empty() {
        anyhow::bail!("{label} is required")
    }
    Ok(value)
}

fn collect_auth(
    profile: &ConnectionProfile,
    fields: &AuthFields,
    allow_keyboard: bool,
) -> anyhow::Result<(AuthConfig, Option<String>, Option<PendingSecret>)> {
    let mode = fields
        .mode
        .active_id()
        .map(|id| id.to_string())
        .unwrap_or_else(|| {
            if allow_keyboard {
                "keyboard".to_string()
            } else {
                "password".to_string()
            }
        });

    match mode.as_str() {
        "keyboard" if allow_keyboard => {
            let username = require_entry_text("Username", &fields.keyboard_username)?;
            Ok((AuthConfig::KeyboardInteractive { username }, None, None))
        }
        "password" => {
            let username = require_entry_text("Username", &fields.password.username)?;
            let password = fields.password.password.text().to_string();
            let (password_ref, pending_secret) = plan_password_storage(
                profile,
                &username,
                &password,
                fields.password.remember.is_active(),
            );
            Ok((
                AuthConfig::Password {
                    username,
                    password_ref,
                },
                (!password.is_empty()).then_some(password),
                pending_secret,
            ))
        }
        "private-key" => {
            let username = require_entry_text("Username", &fields.private_key.username)?;
            let key_path = require_entry_text("Private key", &fields.private_key.key_path)?;
            Ok((
                AuthConfig::PrivateKey {
                    username,
                    key_ref: CredentialsRef::PrivateKey {
                        path: key_path,
                        passphrase_ref: None,
                    },
                },
                None,
                None,
            ))
        }
        other => anyhow::bail!("Unsupported auth mode: {other}"),
    }
}

fn plan_password_storage(
    profile: &ConnectionProfile,
    username: &str,
    password: &str,
    remember: bool,
) -> (CredentialsRef, Option<PendingSecret>) {
    if !remember || password.is_empty() {
        return (CredentialsRef::None, None);
    }

    let reference = CredentialsRef::SystemKeychain {
        service: credential_service(profile),
        account: username.to_string(),
    };
    (
        reference.clone(),
        Some(PendingSecret {
            reference,
            password: password.to_string(),
        }),
    )
}

fn credential_service(profile: &ConnectionProfile) -> String {
    format!(
        "dev.shell.{}.{}",
        protocol_storage_key(&profile.protocol),
        profile.id
    )
}

fn protocol_storage_key(protocol: &ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::LocalShell => "local",
        ProtocolKind::Ssh => "ssh",
        ProtocolKind::Sftp => "sftp",
        ProtocolKind::Ftp => "ftp",
        ProtocolKind::Serial => "serial",
        ProtocolKind::Telnet => "telnet",
        ProtocolKind::Vnc => "vnc",
    }
}

fn run_background_task<T, F, G>(
    state: &AppState,
    pending_status: Option<&str>,
    task: F,
    on_result: G,
) where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    G: FnOnce(&AppState, anyhow::Result<T>) + 'static,
{
    if let Some(message) = pending_status {
        state.status.set_text(message);
    }

    let (sender, receiver) = mpsc::channel::<anyhow::Result<T>>();
    std::thread::spawn(move || {
        let _ = sender.send(task());
    });

    let callback = Rc::new(RefCell::new(Some(on_result)));
    let state_for_result = state.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        match receiver.try_recv() {
            Ok(result) => {
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(&state_for_result, result);
                }
                ControlFlow::Break
            }
            Err(TryRecvError::Empty) => ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => {
                state_for_result
                    .status
                    .set_text("Background task stopped unexpectedly");
                ControlFlow::Break
            }
        }
    });
}

fn store_pending_secret(pending_secret: &PendingSecret) -> anyhow::Result<()> {
    let CredentialsRef::SystemKeychain { service, account } = &pending_secret.reference else {
        return Ok(());
    };

    store_secret(service, account, &pending_secret.password)
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn schedule_secret_delete(state: &AppState, password_ref: CredentialsRef) {
    if matches!(password_ref, CredentialsRef::None) {
        return;
    }

    run_background_task(
        state,
        None,
        move || delete_secret(&password_ref).map_err(|err| anyhow::anyhow!(err.to_string())),
        move |state, result| {
            if let Err(err) = result {
                state
                    .status
                    .set_text(&format!("Failed to delete saved password: {err}"));
            }
        },
    );
}

fn finish_profile_submission(
    state: &AppState,
    submission: ProfileSubmission,
    connect_after_save: bool,
    window: &Window,
) {
    let profile = submission.profile.clone();
    match persist_profile(state, submission.profile) {
        Ok(()) => {
            window.close();
            if connect_after_save {
                open_profile_connection(state, profile, submission.runtime_password);
            } else {
                state.status.set_text("Profile saved");
            }
        }
        Err(err) => state.status.set_text(&err.to_string()),
    }
}

fn submit_profile(
    state: &AppState,
    submission: ProfileSubmission,
    connect_after_save: bool,
    window: &Window,
) {
    if let Some(pending_secret) = submission.pending_secret.clone() {
        let submission_for_result = submission.clone();
        let window_for_result = window.clone();
        run_background_task(
            state,
            Some("Saving profile password..."),
            move || store_pending_secret(&pending_secret),
            move |state, result| match result {
                Ok(()) => finish_profile_submission(
                    state,
                    submission_for_result,
                    connect_after_save,
                    &window_for_result,
                ),
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to store password: {err}")),
            },
        );
        return;
    }

    finish_profile_submission(state, submission, connect_after_save, window);
}

fn finish_profile_update(
    state: &AppState,
    index: usize,
    submission: ProfileSubmission,
    window: &Window,
) {
    match update_profile_at(state, index, submission.profile) {
        Ok(()) => {
            window.close();
            state.status.set_text("Profile updated");
        }
        Err(err) => state.status.set_text(&err.to_string()),
    }
}

fn submit_profile_update(
    state: &AppState,
    index: usize,
    submission: ProfileSubmission,
    window: &Window,
) {
    if let Some(pending_secret) = submission.pending_secret.clone() {
        let submission_for_result = submission.clone();
        let window_for_result = window.clone();
        run_background_task(
            state,
            Some("Saving profile password..."),
            move || store_pending_secret(&pending_secret),
            move |state, result| match result {
                Ok(()) => {
                    finish_profile_update(state, index, submission_for_result, &window_for_result)
                }
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to store password: {err}")),
            },
        );
        return;
    }

    finish_profile_update(state, index, submission, window);
}

fn persist_profile(state: &AppState, profile: ConnectionProfile) -> anyhow::Result<()> {
    state.profiles_doc.borrow_mut().profiles.push(profile);
    state
        .profile_store
        .save(&state.profiles_doc.borrow())
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    refresh_profile_list_for_state(state);
    Ok(())
}

fn update_profile_at(
    state: &AppState,
    index: usize,
    profile: ConnectionProfile,
) -> anyhow::Result<()> {
    let mut document = state.profiles_doc.borrow_mut();
    let Some(slot) = document.profiles.get_mut(index) else {
        anyhow::bail!("Profile no longer exists")
    };
    let previous = slot.clone();
    *slot = profile;

    let replaced_secret = if should_delete_replaced_secret(&previous, slot) {
        match previous.auth {
            AuthConfig::Password { password_ref, .. } => Some(password_ref),
            _ => None,
        }
    } else {
        None
    };

    state
        .profile_store
        .save(&document)
        .map_err(|err| anyhow::anyhow!(err.to_string()))?;
    drop(document);
    refresh_profile_list_for_state(state);
    if let Some(password_ref) = replaced_secret {
        schedule_secret_delete(state, password_ref);
    }
    Ok(())
}

fn should_delete_replaced_secret(
    previous: &ConnectionProfile,
    current: &ConnectionProfile,
) -> bool {
    let AuthConfig::Password {
        password_ref: previous_ref,
        ..
    } = &previous.auth
    else {
        return false;
    };
    let AuthConfig::Password {
        password_ref: current_ref,
        ..
    } = &current.auth
    else {
        return !matches!(previous_ref, CredentialsRef::None);
    };

    previous_ref != current_ref && !matches!(previous_ref, CredentialsRef::None)
}

fn refresh_profile_list_for_state(state: &AppState) {
    refresh_profile_list_internal(
        &state.profiles_list,
        &state.profiles_doc.borrow(),
        Some(state),
    );
}

fn refresh_profile_list_internal(
    list: &ListBox,
    document: &ProfilesDocument,
    state: Option<&AppState>,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    for (index, profile) in document.profiles.iter().enumerate() {
        if matches!(profile.protocol, ProtocolKind::LocalShell) {
            continue;
        }
        let row = make_profile_row(profile, index, state);
        list.append(&row);
    }
}

fn make_profile_row(
    profile: &ConnectionProfile,
    _index: usize,
    state: Option<&AppState>,
) -> ListBoxRow {
    let row = ListBoxRow::new();
    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(8);
    content.set_margin_end(8);

    let text_column = GtkBox::new(Orientation::Vertical, 2);
    text_column.set_hexpand(true);

    let title = Label::new(Some(&profile.name));
    title.set_xalign(0.0);
    let subtitle = Label::new(Some(&profile_summary(profile)));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");

    text_column.append(&title);
    text_column.append(&subtitle);
    content.append(&text_column);

    if let Some(state) = state {
        let sftp_button = Button::new();
        sftp_button.set_has_frame(false);
        sftp_button.add_css_class("profile-sftp-button");
        sftp_button.set_tooltip_text(Some("Open this saved session as SFTP"));
        sftp_button.set_valign(gtk4::Align::Center);
        sftp_button.set_child(Some(&Image::from_icon_name("folder-symbolic")));
        sftp_button.set_sensitive(profile_supports_sftp_shortcut(profile));

        let state_for_sftp = state.clone();
        let profile_for_sftp = profile.clone();
        sftp_button.connect_clicked(move |_| {
            connect_saved_profile_sftp(&state_for_sftp, profile_for_sftp.clone());
        });

        content.append(&sftp_button);
    }

    row.set_child(Some(&content));
    row
}

fn profile_summary(profile: &ConnectionProfile) -> String {
    match profile.protocol {
        ProtocolKind::LocalShell => "Local shell".to_string(),
        ProtocolKind::Serial => format!(
            "Serial • {} @ {}",
            profile.serial_port.as_deref().unwrap_or("unknown"),
            profile.baud_rate.unwrap_or(115_200)
        ),
        _ => format!(
            "{} • {}:{}",
            protocol_label(&profile.protocol),
            profile.host.as_deref().unwrap_or("localhost"),
            profile.port.unwrap_or(default_port_for(&profile.protocol))
        ),
    }
}

fn default_port_for(protocol: &ProtocolKind) -> u16 {
    match protocol {
        ProtocolKind::Ssh | ProtocolKind::Sftp => 22,
        ProtocolKind::Ftp => 21,
        ProtocolKind::Telnet => 23,
        _ => 0,
    }
}

fn delete_profile_at(state: &AppState, index: usize) {
    let mut document = state.profiles_doc.borrow_mut();
    if index >= document.profiles.len() {
        return;
    }

    let profile = document.profiles.remove(index);
    let password_ref = match &profile.auth {
        AuthConfig::Password { password_ref, .. } => Some(password_ref.clone()),
        _ => None,
    };

    if let Err(err) = state.profile_store.save(&document) {
        state
            .status
            .set_text(&format!("Failed to delete profile: {err}"));
    }
    drop(document);
    refresh_profile_list_for_state(state);
    if let Some(password_ref) = password_ref {
        schedule_secret_delete(state, password_ref);
    } else {
        state.status.set_text("Profile deleted");
    }
}

fn connect_saved_profile(state: &AppState, profile: ConnectionProfile) {
    if matches!(profile.protocol, ProtocolKind::LocalShell) {
        open_preferred_local_terminal(state);
        return;
    }

    let needs_password = matches!(profile.auth, AuthConfig::Password { .. })
        && matches!(
            profile.protocol,
            ProtocolKind::Ssh | ProtocolKind::Sftp | ProtocolKind::Ftp
        );

    if !needs_password {
        open_profile_connection(state, profile, None);
        return;
    }

    let AuthConfig::Password { password_ref, .. } = &profile.auth else {
        open_profile_connection(state, profile, None);
        return;
    };

    match password_ref {
        CredentialsRef::None => {
            prompt_for_profile_password(state, profile);
        }
        CredentialsRef::SystemKeychain { .. } => {
            let profile_for_load = profile.clone();
            let profile_for_connect = profile.clone();
            run_background_task(
                state,
                Some("Loading saved password..."),
                move || password_from_profile(&profile_for_load),
                move |state, result| {
                    match result {
                    Ok(Some(password)) => {
                        open_profile_connection(state, profile_for_connect, Some(password));
                    }
                    Ok(None) => state.status.set_text(
                        "Saved password was not found in the secure local vault. Edit the profile and save it again.",
                    ),
                    Err(err) => state.status.set_text(&err.to_string()),
                }
                },
            );
        }
        CredentialsRef::PrivateKey { .. } => {
            open_profile_connection(state, profile, None);
        }
    }
}

fn profile_supports_sftp_shortcut(profile: &ConnectionProfile) -> bool {
    matches!(profile.protocol, ProtocolKind::Ssh | ProtocolKind::Sftp)
}

fn connect_saved_profile_sftp(state: &AppState, profile: ConnectionProfile) {
    if !profile_supports_sftp_shortcut(&profile) {
        state
            .status
            .set_text("SFTP is available for saved SSH and SFTP sessions only");
        return;
    }

    let needs_password = matches!(profile.auth, AuthConfig::Password { .. });
    if !needs_password {
        if let Err(err) = open_sftp_profile_tab(state, profile, None) {
            state
                .status
                .set_text(&format!("Failed to open SFTP: {err}"));
        }
        return;
    }

    let AuthConfig::Password { password_ref, .. } = &profile.auth else {
        if let Err(err) = open_sftp_profile_tab(state, profile, None) {
            state
                .status
                .set_text(&format!("Failed to open SFTP: {err}"));
        }
        return;
    };

    match password_ref {
        CredentialsRef::None => {
            let state_for_prompt = state.clone();
            let profile_for_prompt = profile.clone();
            show_password_prompt(
                &state.window,
                &format!("Connect SFTP {}", profile.name),
                profile.auth.username(),
                move |password| {
                    if let Err(err) = open_sftp_profile_tab(
                        &state_for_prompt,
                        profile_for_prompt.clone(),
                        Some(password),
                    ) {
                        state_for_prompt
                            .status
                            .set_text(&format!("Failed to open SFTP: {err}"));
                    }
                },
            );
        }
        CredentialsRef::SystemKeychain { .. } => {
            let profile_for_load = profile.clone();
            let profile_for_connect = profile.clone();
            run_background_task(
                state,
                Some("Loading saved password..."),
                move || password_from_profile(&profile_for_load),
                move |state, result| {
                    match result {
                    Ok(Some(password)) => {
                        if let Err(err) =
                            open_sftp_profile_tab(state, profile_for_connect, Some(password))
                        {
                            state.status.set_text(&format!("Failed to open SFTP: {err}"));
                        }
                    }
                    Ok(None) => state.status.set_text(
                        "Saved password was not found in the secure local vault. Edit the profile and save it again.",
                    ),
                    Err(err) => state.status.set_text(&err.to_string()),
                }
                },
            );
        }
        CredentialsRef::PrivateKey { .. } => {
            if let Err(err) = open_sftp_profile_tab(state, profile, None) {
                state
                    .status
                    .set_text(&format!("Failed to open SFTP: {err}"));
            }
        }
    }
}

fn prompt_for_profile_password(state: &AppState, profile: ConnectionProfile) {
    let state_for_prompt = state.clone();
    let profile_for_prompt = profile.clone();
    show_password_prompt(
        &state.window,
        &format!("Connect {}", profile.name),
        profile.auth.username(),
        move |password| {
            open_profile_connection(
                &state_for_prompt,
                profile_for_prompt.clone(),
                Some(password),
            );
        },
    );
}

fn password_from_profile(profile: &ConnectionProfile) -> anyhow::Result<Option<String>> {
    let AuthConfig::Password { password_ref, .. } = &profile.auth else {
        return Ok(None);
    };
    load_secret(password_ref).map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn build_ssh_config_from_profile(
    profile: &ConnectionProfile,
    compatibility: &OpensshCompatibilitySettings,
    builtin_tools: &BuiltinToolsSettings,
) -> anyhow::Result<SshConfig> {
    // 集中处理连接配置到 OpenSSH 命令行参数的转换，保证首次连接和页签内重连使用完全一致的
    // 认证参数与兼容性选项。
    let Some(host) = profile.host.as_deref() else {
        anyhow::bail!("SSH host is missing")
    };

    let mut config = SshConfig::new(host);
    config.port = profile.port.unwrap_or(22);
    match &profile.auth {
        AuthConfig::KeyboardInteractive { username } => {
            config.username = Some(username.clone());
            config.extra_args = vec![
                "-o".into(),
                "PreferredAuthentications=keyboard-interactive,password".into(),
            ];
        }
        AuthConfig::Password { username, .. } => {
            config.username = Some(username.clone());
            config.extra_args = vec![
                "-o".into(),
                "PreferredAuthentications=password,keyboard-interactive".into(),
                "-o".into(),
                "PubkeyAuthentication=no".into(),
            ];
        }
        AuthConfig::PrivateKey { username, key_ref } => {
            config.username = Some(username.clone());
            if let CredentialsRef::PrivateKey { path, .. } = key_ref {
                config.identity_file = Some(path.clone());
            }
        }
        AuthConfig::None => {}
    }
    config
        .extra_args
        .extend(openssh_compatibility_args(compatibility));
    apply_builtin_ssh_client(&mut config, builtin_tools);

    Ok(config)
}

fn apply_builtin_ssh_client(config: &mut SshConfig, settings: &BuiltinToolsSettings) {
    if !settings.enabled || !settings.use_for_ssh_client {
        return;
    }
    let Some(toolchain) = active_builtin_toolchain(settings) else {
        return;
    };
    let Some(ssh) = toolchain.command_path("ssh") else {
        return;
    };

    config.client_program = Some(ssh.display().to_string());
    config.launch_options = toolchain.launch_options(ToolchainPathPriority::ToolchainFirst);
}

fn is_enter_input(bytes: &[u8]) -> bool {
    matches!(bytes, b"\r" | b"\n" | b"\r\n")
}

fn ssh_disconnected_input_action(
    disconnected: bool,
    enter_presses: &mut u8,
    bytes: &[u8],
) -> SshDisconnectedInputAction {
    // 保持状态机纯粹且可单元测试：UI 代码只应用返回的动作。断线后的非 Enter 输入会被
    // 吞掉，避免意外写入已关闭的 PTY。
    if !disconnected {
        *enter_presses = 0;
        return SshDisconnectedInputAction::Forward;
    }

    if is_enter_input(bytes) {
        *enter_presses = enter_presses.saturating_add(1);
        if *enter_presses >= 2 {
            *enter_presses = 0;
            SshDisconnectedInputAction::Reconnect
        } else {
            SshDisconnectedInputAction::Hint
        }
    } else {
        *enter_presses = 0;
        SshDisconnectedInputAction::Ignore
    }
}

fn show_password_prompt(
    parent: &ApplicationWindow,
    title: &str,
    username: Option<&str>,
    on_submit: impl Fn(String) + 'static,
) {
    let win = build_modal_window(parent, title, 320, 0);

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);
    if let Some(username) = username {
        let label = Label::new(Some(&format!("Username: {username}")));
        label.set_xalign(0.0);
        root.append(&label);
    }
    let password = new_password_entry("Password");
    let connect_btn = Button::with_label("Connect");
    root.append(&password);
    root.append(&connect_btn);
    win.set_child(Some(&root));

    let win_for_submit = win.clone();
    connect_btn.connect_clicked(move |_| {
        let value = password.text().to_string();
        win_for_submit.close();
        if !value.is_empty() {
            on_submit(value);
        }
    });

    win.present();
}

fn clear_host_stats_poller(state: &AppState) {
    if let Some(source_id) = state.host_stats_poller.borrow_mut().take() {
        source_id.remove();
    }
}

fn reset_host_stats(state: &AppState, text: &str) {
    clear_host_stats_poller(state);
    state.host_info.set_text(text);
}

fn poll_host_stats_once(
    label: &Label,
    host: String,
    port: u16,
    username: String,
    password: String,
    in_flight: Rc<RefCell<bool>>,
) {
    if *in_flight.borrow() {
        return;
    }

    *in_flight.borrow_mut() = true;
    let label_for_result = label.clone();
    let in_flight_for_result = Rc::clone(&in_flight);
    let (sender, receiver) = mpsc::channel::<std::result::Result<String, String>>();
    std::thread::spawn(move || {
        let config = HostStatsConfig {
            host: host.clone(),
            port,
            username,
            password,
        };
        let result = fetch_ssh_host_stats(&config)
            .map(|stats| format_host_stats(&host, &stats))
            .map_err(|err| err.to_string());
        let _ = sender.send(result);
    });

    glib::timeout_add_local(Duration::from_millis(100), move || {
        match receiver.try_recv() {
            Ok(Ok(text)) => {
                *in_flight_for_result.borrow_mut() = false;
                label_for_result.set_text(&text);
                ControlFlow::Break
            }
            Ok(Err(err)) => {
                *in_flight_for_result.borrow_mut() = false;
                label_for_result.set_text(&format!("Host info unavailable: {err}"));
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                *in_flight_for_result.borrow_mut() = false;
                label_for_result.set_text("Host info unavailable: stats worker stopped");
                ControlFlow::Break
            }
        }
    });
}

fn open_profile_connection(state: &AppState, profile: ConnectionProfile, password: Option<String>) {
    match profile.protocol {
        ProtocolKind::LocalShell => open_preferred_local_terminal(state),
        ProtocolKind::Ssh => {
            let settings = state.app_settings.borrow();
            let config = match build_ssh_config_from_profile(
                &profile,
                &settings.openssh_compatibility,
                &settings.builtin_tools,
            ) {
                Ok(config) => config,
                Err(err) => {
                    state.status.set_text(&err.to_string());
                    return;
                }
            };
            drop(settings);

            let host_for_stats = config.host.clone();
            let port_for_stats = config.port;
            let username_for_stats = profile.auth.username().map(str::to_string);
            let password_for_stats = password.clone();
            match spawn_ssh_tab(state, config, profile.clone(), password) {
                Ok(connection) => {
                    state.connections.borrow_mut().ssh.push(connection);
                    request_host_stats(
                        state,
                        host_for_stats,
                        port_for_stats,
                        username_for_stats,
                        password_for_stats,
                    );
                }
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to start SSH: {err}")),
            }
        }
        ProtocolKind::Sftp => {
            if let Err(err) = open_sftp_profile_tab(state, profile, password) {
                state
                    .status
                    .set_text(&format!("Failed to open SFTP: {err}"));
            }
        }
        ProtocolKind::Ftp => {
            reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
            let Some(host) = profile.host.as_deref() else {
                state.status.set_text("FTP host is missing");
                return;
            };
            let mut config = FtpConfig::new(host);
            config.port = profile.port.unwrap_or(21);
            config.username = profile.auth.username().unwrap_or("anonymous").to_string();
            config.password = password.unwrap_or_else(|| "guest@example.com".to_string());

            match spawn_ftp_tab(state, config) {
                Ok(connection) => state.connections.borrow_mut().ftp.push(connection),
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to connect FTP: {err}")),
            }
        }
        ProtocolKind::Telnet => {
            reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
            let Some(host) = profile.host.as_deref() else {
                state.status.set_text("Telnet host is missing");
                return;
            };
            let mut config = TelnetConfig::new(host);
            config.port = profile.port.unwrap_or(23);
            match spawn_telnet_tab(state, config) {
                Ok(connection) => state.connections.borrow_mut().telnet.push(connection),
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to start Telnet: {err}")),
            }
        }
        ProtocolKind::Serial => {
            reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
            let Some(port_name) = profile.serial_port.as_deref() else {
                state.status.set_text("Serial port is missing");
                return;
            };
            let mut config = SerialConfig::new(port_name);
            config.baud_rate = profile.baud_rate.unwrap_or(115_200);
            match spawn_serial_tab(state, config) {
                Ok(connection) => state.connections.borrow_mut().serial.push(connection),
                Err(err) => state
                    .status
                    .set_text(&format!("Failed to open serial: {err}")),
            }
        }
        ProtocolKind::Vnc => {
            reset_host_stats(state, "Host: -    CPU: -    Mem: -    Net: -    Uptime: -");
            state.status.set_text("VNC is not implemented yet");
        }
    }
}

fn openssh_compatibility_args(settings: &OpensshCompatibilitySettings) -> Vec<String> {
    if !settings.enabled {
        return Vec::new();
    }

    let mut options = Vec::new();
    if settings.rsa_sha1 || settings.legacy_openssh {
        append_openssh_option(&mut options, "HostKeyAlgorithms", "+ssh-rsa");
        append_openssh_option(&mut options, "PubkeyAcceptedKeyTypes", "+ssh-rsa");
    }
    if settings.regional_crypto {
        append_openssh_option(
            &mut options,
            "KexAlgorithms",
            "+sm2kep-sha256,ecdh-sm2-nistp256,sm2dh-sha256",
        );
        append_openssh_option(&mut options, "HostKeyAlgorithms", "+sm2sig_sm3,ssh-sm2,sm2");
        append_openssh_option(
            &mut options,
            "PubkeyAcceptedKeyTypes",
            "+sm2sig_sm3,ssh-sm2,sm2",
        );
        append_openssh_option(&mut options, "Ciphers", "+sm4-ctr,sm4-cbc");
        append_openssh_option(&mut options, "MACs", "+hmac-sm3");
    }
    if settings.dss_host_key || settings.weak_kex {
        append_openssh_option(&mut options, "HostKeyAlgorithms", "+ssh-dss");
        append_openssh_option(&mut options, "PubkeyAcceptedKeyTypes", "+ssh-dss");
    }
    if settings.legacy_kex || settings.weak_kex {
        append_openssh_option(
            &mut options,
            "KexAlgorithms",
            "+diffie-hellman-group14-sha1,diffie-hellman-group1-sha1",
        );
    }
    if settings.legacy_ciphers_macs || settings.weak_kex {
        append_openssh_option(&mut options, "Ciphers", "+aes128-cbc,3des-cbc");
        append_openssh_option(&mut options, "MACs", "+hmac-sha1,hmac-md5");
    }

    options
        .into_iter()
        .flat_map(|(key, value)| ["-o".to_string(), format!("{key}={value}")])
        .collect()
}

fn append_openssh_option(
    options: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: &str,
) {
    let next = value.strip_prefix('+').unwrap_or(value);
    if let Some((_, existing)) = options
        .iter_mut()
        .find(|(existing_key, _)| *existing_key == key)
    {
        if !existing.ends_with(',') {
            existing.push(',');
        }
        existing.push_str(next);
        return;
    }

    options.push((key, value.to_string()));
}

fn request_host_stats(
    state: &AppState,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
) {
    let Some(username) = username else {
        reset_host_stats(
            state,
            "Host: unavailable    CPU: -    Mem: -    Net: -    Uptime: -",
        );
        return;
    };
    let Some(password) = password else {
        reset_host_stats(
            state,
            "Host: connected    CPU: -    Mem: -    Net: -    Uptime: -    Host info needs password auth",
        );
        return;
    };

    clear_host_stats_poller(state);
    state.host_info.set_text("Host: loading remote metrics...");
    let label = state.host_info.clone();
    let in_flight = Rc::new(RefCell::new(false));
    poll_host_stats_once(
        &label,
        host.clone(),
        port,
        username.clone(),
        password.clone(),
        Rc::clone(&in_flight),
    );

    let source_id = glib::timeout_add_local(Duration::from_secs(5), move || {
        poll_host_stats_once(
            &label,
            host.clone(),
            port,
            username.clone(),
            password.clone(),
            Rc::clone(&in_flight),
        );
        ControlFlow::Continue
    });
    *state.host_stats_poller.borrow_mut() = Some(source_id);
}

fn spawn_local_tab(
    state: &AppState,
    terminal: &LocalTerminalProfile,
) -> anyhow::Result<Rc<LocalShellConnection>> {
    let size = TerminalSize::new(120, 32);
    let buffer = TerminalBuffer::new(size, DEFAULT_SCROLLBACK_LINES);
    let view = GtkTerminalView::with_appearance(buffer, state.terminal_appearance.clone());
    install_terminal_font_zoom_handler(state, &view);
    let widget = view.widget();
    register_terminal_widget(state, &widget);

    let launch_options = local_terminal_launch_options(&state.app_settings.borrow(), terminal);
    let (connection, receiver) = LocalShellConnection::spawn_with_options(
        size,
        terminal.program.clone(),
        terminal.args.clone(),
        launch_options,
    )?;
    let connection = Rc::new(connection);
    let input_connection = Rc::clone(&connection);
    view.set_input_handler(move |bytes| {
        if let Err(err) = input_connection.send_input(&bytes) {
            tracing::warn!("failed to send input to local shell: {err}");
        }
    });
    let resize_connection = Rc::clone(&connection);
    view.set_resize_handler(move |size| {
        if let Err(err) = resize_connection.resize(size) {
            tracing::warn!("failed to resize local shell: {err}");
        }
    });

    attach_protocol_receiver(
        view.clone(),
        receiver,
        state.status.clone(),
        "Local terminal",
        None,
        None,
    );

    let notebook_label = Label::new(None);
    notebook_label.set_size_request(1, 1);
    state.notebook.append_page(&widget, Some(&notebook_label));
    register_page_tab(state, &widget, terminal.title.clone());
    let view_for_shutdown = view.clone();
    let connection_for_shutdown = Rc::clone(&connection);
    register_page_shutdown(state, &widget, move || {
        view_for_shutdown.clear_io_handlers();
        let _ = connection_for_shutdown.shutdown();
    });
    activate_session_page(state, widget.upcast_ref());

    Ok(connection)
}

fn make_ssh_password_observer(connection: Rc<SshConnection>, password: String) -> OutputObserver {
    let mut pending_password = Some(password.into_bytes());
    let mut recent_output = String::new();

    Rc::new(RefCell::new(Some(Box::new(move |bytes: &[u8]| {
        let Some(password_bytes) = pending_password.as_ref() else {
            return;
        };

        recent_output.push_str(&String::from_utf8_lossy(bytes).to_lowercase());
        if recent_output.len() > 512 {
            let trim_from = recent_output.len().saturating_sub(512);
            recent_output.drain(..trim_from);
        }

        if recent_output.contains("password:") || recent_output.contains("password for") {
            let _ = connection.send_input(password_bytes);
            let _ = connection.send_input(b"\r");
            pending_password = None;
        }
    }))))
}

fn mark_ssh_session_disconnected(state: &AppState, session: &SshReconnectSession, reason: &str) {
    // 发送失败、PTY EOF、读取线程错误或通道关闭都可能调用这里。只有第一次进入
    // 断线状态时打印用户提示，重复通知会被忽略，避免刷屏。
    if state
        .notebook
        .page_num(session.page.upcast_ref::<Widget>())
        .is_none()
    {
        return;
    }

    let was_disconnected = session.disconnected.replace(true);
    session.reconnecting.set(false);
    session.enter_presses.set(0);
    if was_disconnected {
        return;
    }

    let language = state.app_settings.borrow().language.clone();
    let prompt = tr(
        &language,
        "连续按两次回车重新连接",
        "Press Enter twice to reconnect",
    );
    let message = format!(
        "\r\n[{}: {reason}. {prompt}.]\r\n",
        tr(&language, "SSH 连接已断开", "SSH disconnected"),
    );
    session.view.feed(message.as_bytes());
    state.status.set_text(&format!(
        "{}: {reason}. {prompt}.",
        tr(&language, "SSH 连接已断开", "SSH disconnected"),
    ));
}

fn attach_ssh_protocol_receiver(
    state: &AppState,
    session: &SshReconnectSession,
    receiver: mpsc::Receiver<ProtocolEvent>,
    connection: Rc<SshConnection>,
) {
    let observer = session
        .runtime_password
        .clone()
        .map(|password| make_ssh_password_observer(connection, password));
    let state_for_disconnect = state.clone();
    let session_for_disconnect = session.clone();
    let disconnect_observer: DisconnectObserver = Rc::new(move |reason| {
        mark_ssh_session_disconnected(&state_for_disconnect, &session_for_disconnect, reason);
    });

    attach_protocol_receiver(
        session.view.clone(),
        receiver,
        state.status.clone(),
        "SSH",
        observer,
        Some(disconnect_observer),
    );
}

fn reconnect_ssh_session(state: &AppState, session: &SshReconnectSession) {
    // 在现有页签内重连：保持终端缓冲区和页面可见，只替换底层 SSH PTY 对象，并挂接
    // 新的 receiver。
    if state
        .notebook
        .page_num(session.page.upcast_ref::<Widget>())
        .is_none()
    {
        return;
    }
    if session.reconnecting.replace(true) {
        return;
    }

    let language = state.app_settings.borrow().language.clone();
    state
        .status
        .set_text(tr(&language, "正在重新连接 SSH...", "Reconnecting SSH..."));
    session.view.feed(
        format!(
            "\r\n[{}]\r\n",
            tr(&language, "正在重新连接 SSH...", "Reconnecting SSH...")
        )
        .as_bytes(),
    );

    if let Some(connection) = session.connection.borrow_mut().take() {
        let _ = connection.shutdown();
    }
    schedule_session_handle_prune(state);

    let settings = state.app_settings.borrow();
    let config = match build_ssh_config_from_profile(
        &session.profile,
        &settings.openssh_compatibility,
        &settings.builtin_tools,
    ) {
        Ok(config) => config,
        Err(err) => {
            session.disconnected.set(true);
            session.reconnecting.set(false);
            state.status.set_text(&format!(
                "{}: {err}",
                tr(&language, "SSH 重连失败", "SSH reconnect failed")
            ));
            session.view.feed(
                format!(
                    "\r\n[{}: {err}. {}]\r\n",
                    tr(&language, "SSH 重连失败", "SSH reconnect failed"),
                    tr(
                        &language,
                        "连续按两次回车重试",
                        "Press Enter twice to retry"
                    ),
                )
                .as_bytes(),
            );
            return;
        }
    };
    drop(settings);

    let host_for_stats = config.host.clone();
    let port_for_stats = config.port;
    let username_for_stats = session.profile.auth.username().map(str::to_string);
    let password_for_stats = session.runtime_password.clone();
    match SshConnection::spawn(config, session.last_size.get()) {
        Ok((connection, receiver)) => {
            let connection = Rc::new(connection);
            *session.connection.borrow_mut() = Some(Rc::clone(&connection));
            state
                .connections
                .borrow_mut()
                .ssh
                .push(Rc::clone(&connection));
            attach_ssh_protocol_receiver(state, session, receiver, Rc::clone(&connection));
            session.disconnected.set(false);
            session.reconnecting.set(false);
            session.enter_presses.set(0);
            session.view.feed(
                format!(
                    "\r\n[{}]\r\n",
                    tr(&language, "SSH 已重新连接", "SSH reconnected")
                )
                .as_bytes(),
            );
            state
                .status
                .set_text(tr(&language, "SSH 已重新连接", "SSH reconnected"));
            request_host_stats(
                state,
                host_for_stats,
                port_for_stats,
                username_for_stats,
                password_for_stats,
            );
        }
        Err(err) => {
            session.disconnected.set(true);
            session.reconnecting.set(false);
            state.status.set_text(&format!(
                "{}: {err}",
                tr(&language, "SSH 重连失败", "SSH reconnect failed")
            ));
            session.view.feed(
                format!(
                    "\r\n[{}: {err}. {}]\r\n",
                    tr(&language, "SSH 重连失败", "SSH reconnect failed"),
                    tr(
                        &language,
                        "连续按两次回车重试",
                        "Press Enter twice to retry"
                    ),
                )
                .as_bytes(),
            );
        }
    }
}

fn spawn_ssh_tab(
    state: &AppState,
    config: SshConfig,
    profile: ConnectionProfile,
    password: Option<String>,
) -> anyhow::Result<Rc<SshConnection>> {
    // SSH 通过 PTY 中的系统 `ssh` 客户端运行。这样 host-key 提示和 OpenSSH 特殊行为
    // 与用户机器保持一致，同时应用仍负责尺寸调整、输入转发和重连状态。
    let size = TerminalSize::new(120, 32);
    let buffer = TerminalBuffer::new(size, DEFAULT_SCROLLBACK_LINES);
    let view = GtkTerminalView::with_appearance(buffer, state.terminal_appearance.clone());
    install_terminal_font_zoom_handler(state, &view);
    let widget = view.widget();
    register_terminal_widget(state, &widget);
    let tab_title = format!("SSH {}", config.target());

    let (connection, receiver) = SshConnection::spawn(config, size)?;
    let connection = Rc::new(connection);
    let runtime_password = password.clone();
    let reconnect_session = SshReconnectSession {
        view: view.clone(),
        page: widget.clone(),
        profile: profile.clone(),
        runtime_password: runtime_password.clone(),
        connection: Rc::new(RefCell::new(Some(Rc::clone(&connection)))),
        disconnected: Rc::new(Cell::new(false)),
        enter_presses: Rc::new(Cell::new(0)),
        reconnecting: Rc::new(Cell::new(false)),
        last_size: Rc::new(Cell::new(size)),
    };
    let state_for_input = state.clone();
    let session_for_input = reconnect_session.clone();
    view.set_input_handler(move |bytes| {
        if session_for_input.reconnecting.get() {
            return;
        }

        let mut enter_presses = session_for_input.enter_presses.get();
        let action = ssh_disconnected_input_action(
            session_for_input.disconnected.get(),
            &mut enter_presses,
            &bytes,
        );
        session_for_input.enter_presses.set(enter_presses);

        match action {
            SshDisconnectedInputAction::Forward => {
                let Some(connection) = session_for_input.connection.borrow().clone() else {
                    mark_ssh_session_disconnected(
                        &state_for_input,
                        &session_for_input,
                        "PTY is closed",
                    );
                    return;
                };
                if let Err(err) = connection.send_input(&bytes) {
                    tracing::warn!("failed to send input to SSH session: {err}");
                    mark_ssh_session_disconnected(
                        &state_for_input,
                        &session_for_input,
                        &err.to_string(),
                    );
                }
            }
            SshDisconnectedInputAction::Hint => {
                let language = state_for_input.app_settings.borrow().language.clone();
                let message = tr(
                    &language,
                    "SSH 已断开，再按一次回车重连。",
                    "SSH is disconnected; press Enter once more to reconnect.",
                );
                session_for_input
                    .view
                    .feed(format!("\r\n[{message}]\r\n").as_bytes());
                state_for_input.status.set_text(message);
            }
            SshDisconnectedInputAction::Reconnect => {
                reconnect_ssh_session(&state_for_input, &session_for_input);
            }
            SshDisconnectedInputAction::Ignore => {
                let language = state_for_input.app_settings.borrow().language.clone();
                state_for_input.status.set_text(tr(
                    &language,
                    "SSH 已断开，连续按两次回车重连。",
                    "SSH is disconnected; press Enter twice to reconnect.",
                ));
            }
        }
    });
    let session_for_resize = reconnect_session.clone();
    view.set_resize_handler(move |size| {
        session_for_resize.last_size.set(size);
        let Some(connection) = session_for_resize.connection.borrow().clone() else {
            return;
        };
        if let Err(err) = connection.resize(size) {
            tracing::warn!("failed to resize SSH session: {err}");
        }
    });

    attach_ssh_protocol_receiver(state, &reconnect_session, receiver, Rc::clone(&connection));

    let tab_label = make_tab_label(&tab_title, &state.notebook, &widget);
    state.notebook.append_page(&widget, Some(&tab_label));
    register_page_context(state, &widget, profile, runtime_password);
    register_page_tab(state, &widget, tab_title.clone());
    let view_for_shutdown = view.clone();
    let session_for_shutdown = reconnect_session.clone();
    register_page_shutdown(state, &widget, move || {
        view_for_shutdown.clear_io_handlers();
        session_for_shutdown.disconnected.set(true);
        if let Some(connection) = session_for_shutdown.connection.borrow_mut().take() {
            let _ = connection.shutdown();
        }
    });
    activate_session_page(state, widget.upcast_ref());

    Ok(connection)
}

fn spawn_telnet_tab(
    state: &AppState,
    config: TelnetConfig,
) -> anyhow::Result<Rc<TelnetConnection>> {
    let size = TerminalSize::new(120, 32);
    let buffer = TerminalBuffer::new(size, DEFAULT_SCROLLBACK_LINES);
    let view = GtkTerminalView::with_appearance(buffer, state.terminal_appearance.clone());
    install_terminal_font_zoom_handler(state, &view);
    let widget = view.widget();
    register_terminal_widget(state, &widget);
    let tab_title = format!("telnet {}", config.endpoint());

    let (connection, receiver) = TelnetConnection::connect(config, size)?;
    let connection = Rc::new(connection);
    let input_connection = Rc::clone(&connection);
    view.set_input_handler(move |bytes| {
        if let Err(err) = input_connection.send_input(&bytes) {
            tracing::warn!("failed to send input to Telnet session: {err}");
        }
    });
    let resize_connection = Rc::clone(&connection);
    view.set_resize_handler(move |size| {
        if let Err(err) = resize_connection.resize(size) {
            tracing::warn!("failed to resize Telnet session: {err}");
        }
    });

    attach_protocol_receiver(
        view.clone(),
        receiver,
        state.status.clone(),
        "Telnet",
        None,
        None,
    );

    let tab_label = make_tab_label(&tab_title, &state.notebook, &widget);
    state.notebook.append_page(&widget, Some(&tab_label));
    register_page_tab(state, &widget, tab_title.clone());
    let view_for_shutdown = view.clone();
    let connection_for_shutdown = Rc::clone(&connection);
    register_page_shutdown(state, &widget, move || {
        view_for_shutdown.clear_io_handlers();
        let _ = connection_for_shutdown.shutdown();
    });
    activate_session_page(state, widget.upcast_ref());

    Ok(connection)
}

fn spawn_serial_tab(
    state: &AppState,
    config: SerialConfig,
) -> anyhow::Result<Rc<SerialConnection>> {
    let size = TerminalSize::new(120, 32);
    let buffer = TerminalBuffer::new(size, DEFAULT_SCROLLBACK_LINES);
    let view = GtkTerminalView::with_appearance(buffer, state.terminal_appearance.clone());
    install_terminal_font_zoom_handler(state, &view);
    let widget = view.widget();
    register_terminal_widget(state, &widget);
    let tab_title = format!("serial {}", config.port_name);

    let (connection, receiver) = SerialConnection::open(config)?;
    let connection = Rc::new(connection);
    let input_connection = Rc::clone(&connection);
    view.set_input_handler(move |bytes| {
        if let Err(err) = input_connection.send_input(&bytes) {
            tracing::warn!("failed to send input to serial session: {err}");
        }
    });

    attach_protocol_receiver(
        view.clone(),
        receiver,
        state.status.clone(),
        "Serial",
        None,
        None,
    );

    let tab_label = make_tab_label(&tab_title, &state.notebook, &widget);
    state.notebook.append_page(&widget, Some(&tab_label));
    register_page_tab(state, &widget, tab_title.clone());
    let view_for_shutdown = view.clone();
    let connection_for_shutdown = Rc::clone(&connection);
    register_page_shutdown(state, &widget, move || {
        view_for_shutdown.clear_io_handlers();
        let _ = connection_for_shutdown.shutdown();
    });
    activate_session_page(state, widget.upcast_ref());

    Ok(connection)
}

fn spawn_ftp_tab(state: &AppState, config: FtpConfig) -> anyhow::Result<Rc<FtpConnection>> {
    let size = TerminalSize::new(120, 32);
    let buffer = TerminalBuffer::new(size, DEFAULT_SCROLLBACK_LINES);
    let view = GtkTerminalView::with_appearance(buffer, state.terminal_appearance.clone());
    install_terminal_font_zoom_handler(state, &view);
    let widget = view.widget();
    register_terminal_widget(state, &widget);
    let tab_title = format!("ftp {}", config.host);

    let (connection, receiver) = FtpConnection::connect(config)?;
    let connection = Rc::new(connection);
    let input_connection = Rc::clone(&connection);
    view.set_input_handler(move |bytes| {
        if let Err(err) = input_connection.send_input(&bytes) {
            tracing::warn!("failed to send input to FTP session: {err}");
        }
    });

    attach_protocol_receiver(
        view.clone(),
        receiver,
        state.status.clone(),
        "FTP",
        None,
        None,
    );

    let tab_label = make_tab_label(&tab_title, &state.notebook, &widget);
    state.notebook.append_page(&widget, Some(&tab_label));
    register_page_tab(state, &widget, tab_title.clone());
    let view_for_shutdown = view.clone();
    let connection_for_shutdown = Rc::clone(&connection);
    register_page_shutdown(state, &widget, move || {
        view_for_shutdown.clear_io_handlers();
        let _ = connection_for_shutdown.shutdown();
    });
    activate_session_page(state, widget.upcast_ref());

    Ok(connection)
}

fn attach_protocol_receiver(
    view: GtkTerminalView,
    receiver: mpsc::Receiver<ProtocolEvent>,
    status: Label,
    label: &'static str,
    output_observer: Option<OutputObserver>,
    disconnect_observer: Option<DisconnectObserver>,
) {
    // 在 GTK 主循环中轮询，并且每 tick 只处理有限批量。即使命令瞬间输出大量数据，
    // 也能保持合理 UI 延迟，同时避免协议读取线程直接访问 GTK。
    glib::timeout_add_local(Duration::from_millis(16), move || {
        for _ in 0..128 {
            let event = match receiver.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if let Some(observer) = disconnect_observer.as_ref() {
                        observer("event channel closed");
                    }
                    return ControlFlow::Break;
                }
            };

            match event {
                ProtocolEvent::Output(bytes) => {
                    if let Some(observer) = output_observer.as_ref()
                        && let Some(callback) = observer.borrow_mut().as_mut()
                    {
                        callback(&bytes);
                    }
                    view.feed(&bytes);
                }
                ProtocolEvent::Exited(code) => {
                    let reason = format!("exited: {code:?}");
                    status.set_text(&format!("{label} {reason}"));
                    if let Some(observer) = disconnect_observer.as_ref() {
                        observer(&reason);
                    }
                    return ControlFlow::Break;
                }
                ProtocolEvent::Error(message) => {
                    status.set_text(&format!("{label} error: {message}"));
                    if let Some(observer) = disconnect_observer.as_ref() {
                        observer(&format!("error: {message}"));
                    }
                    return ControlFlow::Break;
                }
            }
        }
        ControlFlow::Continue
    });
}

fn profiles_path() -> PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("shell")
        .join("profiles.json")
}

fn default_profiles_export_path() -> PathBuf {
    profiles_path().with_file_name("profiles-export.json")
}

fn protocol_label(kind: &ProtocolKind) -> &'static str {
    match kind {
        ProtocolKind::LocalShell => "Local",
        ProtocolKind::Ssh => "SSH",
        ProtocolKind::Sftp => "SFTP",
        ProtocolKind::Ftp => "FTP",
        ProtocolKind::Serial => "Serial",
        ProtocolKind::Telnet => "Telnet",
        ProtocolKind::Vnc => "VNC",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_tab_target_width_uses_max_width_for_small_tab_counts() {
        assert_eq!(session_tab_target_width(960, 2), SESSION_TAB_MAX_WIDTH,);
    }

    #[test]
    fn session_tab_target_width_shrinks_before_reaching_min_width() {
        assert_eq!(session_tab_target_width(960, 5), 187,);
    }

    #[test]
    fn session_tab_target_width_clamps_to_min_width() {
        assert_eq!(session_tab_target_width(240, 4), SESSION_TAB_MIN_WIDTH,);
        assert_eq!(session_tab_target_width(640, 16), SESSION_TAB_MIN_WIDTH,);
    }

    #[test]
    fn session_tab_content_width_counts_tabs_and_gaps() {
        assert_eq!(session_tab_content_width(SESSION_TAB_MAX_WIDTH, 0), 0);
        assert_eq!(
            session_tab_content_width(SESSION_TAB_MAX_WIDTH, 1),
            SESSION_TAB_MAX_WIDTH,
        );
        assert_eq!(session_tab_content_width(120, 3), 372);
    }

    #[test]
    fn session_tab_effective_available_width_ignores_stale_scroll_width() {
        assert_eq!(session_tab_effective_available_width(640, 220), 640);
        assert_eq!(session_tab_effective_available_width(640, 0), 640);
    }

    #[test]
    fn session_tab_min_width_keeps_close_button_visible() {
        let label_width = session_tab_button_width(SESSION_TAB_MIN_WIDTH);

        assert!(label_width >= SESSION_TAB_LABEL_MIN_WIDTH);
        assert!(
            SESSION_TAB_MIN_WIDTH
                >= label_width + SESSION_TAB_CLOSE_WIDTH + SESSION_TAB_INNER_CHROME_WIDTH
        );
    }

    #[test]
    fn disconnected_ssh_double_enter_reconnects() {
        let mut enter_presses = 0;

        assert_eq!(
            ssh_disconnected_input_action(true, &mut enter_presses, b"\r"),
            SshDisconnectedInputAction::Hint,
        );
        assert_eq!(enter_presses, 1);
        assert_eq!(
            ssh_disconnected_input_action(true, &mut enter_presses, b"\r"),
            SshDisconnectedInputAction::Reconnect,
        );
        assert_eq!(enter_presses, 0);
    }

    #[test]
    fn disconnected_ssh_non_enter_resets_sequence() {
        let mut enter_presses = 1;

        assert_eq!(
            ssh_disconnected_input_action(true, &mut enter_presses, b"a"),
            SshDisconnectedInputAction::Ignore,
        );
        assert_eq!(enter_presses, 0);
        assert_eq!(
            ssh_disconnected_input_action(false, &mut enter_presses, b"\r"),
            SshDisconnectedInputAction::Forward,
        );
    }

    #[test]
    fn sftp_config_requires_password_for_password_auth() {
        let mut profile = ConnectionProfile::new("ssh demo", ProtocolKind::Ssh);
        profile.host = Some("192.168.6.111".into());
        profile.auth = AuthConfig::Password {
            username: "root".into(),
            password_ref: CredentialsRef::SystemKeychain {
                service: "svc".into(),
                account: "acct".into(),
            },
        };

        let err = build_sftp_config_from_profile(&profile, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not have a reusable password")
        );
    }

    #[test]
    fn sftp_config_accepts_private_key_auth() {
        let mut profile = ConnectionProfile::new("ssh demo", ProtocolKind::Ssh);
        profile.host = Some("192.168.6.111".into());
        profile.port = Some(2222);
        profile.auth = AuthConfig::PrivateKey {
            username: "root".into(),
            key_ref: CredentialsRef::PrivateKey {
                path: "C:/keys/id_ed25519".into(),
                passphrase_ref: None,
            },
        };

        let config = build_sftp_config_from_profile(&profile, None).unwrap();
        assert_eq!(config.host, "192.168.6.111");
        assert_eq!(config.port, 2222);
        assert!(config.password.is_none());
    }

    #[test]
    fn openssh_compatibility_args_are_empty_when_disabled() {
        let settings = OpensshCompatibilitySettings {
            enabled: false,
            rsa_sha1: true,
            dss_host_key: true,
            legacy_kex: true,
            legacy_ciphers_macs: true,
            legacy_openssh: true,
            regional_crypto: true,
            weak_kex: true,
        };

        assert!(openssh_compatibility_args(&settings).is_empty());
    }

    #[test]
    fn openssh_compatibility_args_merge_algorithm_groups() {
        let settings = OpensshCompatibilitySettings {
            enabled: true,
            rsa_sha1: true,
            dss_host_key: true,
            legacy_kex: true,
            legacy_ciphers_macs: true,
            legacy_openssh: true,
            regional_crypto: true,
            weak_kex: true,
        };

        let args = openssh_compatibility_args(&settings);

        assert_eq!(args.iter().filter(|arg| arg.as_str() == "-o").count(), 5);
        assert!(args.contains(&"HostKeyAlgorithms=+ssh-rsa,sm2sig_sm3,ssh-sm2,sm2,ssh-dss".into()));
        assert!(
            args.contains(&"PubkeyAcceptedKeyTypes=+ssh-rsa,sm2sig_sm3,ssh-sm2,sm2,ssh-dss".into())
        );
        assert!(args.contains(&"KexAlgorithms=+sm2kep-sha256,ecdh-sm2-nistp256,sm2dh-sha256,diffie-hellman-group14-sha1,diffie-hellman-group1-sha1".into()));
        assert!(args.contains(&"Ciphers=+sm4-ctr,sm4-cbc,aes128-cbc,3des-cbc".into()));
        assert!(args.contains(&"MACs=+hmac-sm3,hmac-sha1,hmac-md5".into()));
    }

    #[test]
    fn formats_unix_permissions() {
        assert_eq!(format_permissions(Some(0o755), true), "drwxr-xr-x");
        assert_eq!(format_permissions(Some(0o644), false), "-rw-r--r--");
    }

    #[test]
    fn formats_unix_timestamp_as_utc_date() {
        assert_eq!(format_unix_timestamp(0), "-");
        assert_eq!(format_unix_timestamp(1_704_067_200), "2024-01-01 00:00");
    }

    #[test]
    fn parses_octal_permissions() {
        assert_eq!(parse_octal_permissions("755").unwrap(), 0o755);
        assert_eq!(parse_octal_permissions("0o644").unwrap(), 0o644);
        assert!(parse_octal_permissions("999").is_err());
    }

    #[test]
    fn paste_names_preserve_extensions() {
        assert_eq!(pasted_name_for("app.log", false), "app copy.log");
        assert_eq!(pasted_name_for("config", false), "config copy");
        assert_eq!(pasted_name_for("folder", true), "folder copy");
    }
}
