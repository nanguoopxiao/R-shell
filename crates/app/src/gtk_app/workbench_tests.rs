use super::*;

fn settle_layout() {
    let context = glib::MainContext::default();
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    while std::time::Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn save_snapshot(window: &ApplicationWindow, name: &str) {
    let Some(directory) = std::env::var_os("SHELL_APP_UI_SNAPSHOTS") else {
        return;
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).unwrap();
    let snapshot = gtk4::Snapshot::new();
    gtk4::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let node = snapshot.to_node().expect("the workbench must render");
    let viewport =
        gtk4::graphene::Rect::new(0.0, 0.0, window.width() as f32, window.height() as f32);
    window
        .renderer()
        .expect("native renderer")
        .render_texture(&node, Some(&viewport))
        .save_to_png(directory.join(format!("{name}.png")))
        .unwrap();
}

fn fixture_profile(name: &str, group: &str, host: &str) -> ConnectionProfile {
    let mut profile = ConnectionProfile::new(name, ProtocolKind::Ssh);
    profile.host = Some(host.into());
    profile.group = Some(group.into());
    profile.auth = AuthConfig::KeyboardInteractive {
        username: "deploy".into(),
    };
    profile
}

#[test]
#[ignore = "requires a GTK display; run explicitly with --ignored --test-threads=1"]
fn workbench_layout_search_focus_and_native_snapshot() {
    gtk4::init().expect("GTK display");
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(true);
        settings.set_gtk_enable_animations(false);
    }
    let errors = Rc::new(RefCell::new(Vec::new()));
    let parse_errors = Rc::clone(&errors);
    let provider = CssProvider::new();
    provider.connect_parsing_error(move |_, _, error| {
        parse_errors.borrow_mut().push(error.to_string())
    });
    provider.load_from_data(include_str!("../../resources/workbench.css"));
    assert!(
        errors.borrow().is_empty(),
        "CSS errors: {:?}",
        errors.borrow()
    );
    gtk4::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let app = Application::builder()
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gio::Cancellable>).unwrap();
    let language = AppLanguage::ZhCn;
    let tabs = build_session_tab_strip(&language);
    let profiles = ListBox::new();
    profiles.add_css_class("profiles-list");
    profiles.set_selection_mode(SelectionMode::Single);
    let files = build_sftp_sidebar(false, &language);
    let notebook = Notebook::new();
    notebook.set_show_tabs(false);
    notebook.set_show_border(false);
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    let workspace = Stack::new();
    workspace.set_hhomogeneous(false);
    workspace.set_vhomogeneous(false);
    workspace.add_named(&notebook, Some(WORKSPACE_PAGE_SESSIONS));
    let (empty, _, _) = build_empty_workspace(&language);
    workspace.add_named(&empty, Some(WORKSPACE_PAGE_EMPTY));
    let status = Label::new(Some("就绪"));
    status.set_xalign(0.0);
    let host_info = Label::new(Some("CPU 2.3% · RAM 1.8 / 8 GB · ↓ 12 KB/s ↑ 3 KB/s"));
    let settings = AppSettings::default();
    let appearance = TerminalAppearance::new(settings.terminal_font.clone());
    let chrome = Workbench::new(
        &language,
        WorkbenchParts {
            tabs: &tabs,
            profiles: &profiles,
            files: &files,
            workspace: &workspace,
            status: &status,
            host_info: &host_info,
        },
        settings.workspace_layout.clone(),
    );
    let window = ApplicationWindow::builder()
        .application(&app)
        .title("R-shell · UI test")
        .default_width(1280)
        .default_height(780)
        .child(&chrome.root)
        .build();
    window.set_titlebar(Some(&chrome.header));
    window.add_css_class("app-window");
    let document = ProfilesDocument {
        profiles: vec![
            fixture_profile("Production API", "生产环境", "192.0.2.10"),
            fixture_profile("Database", "生产环境", "192.0.2.20"),
            fixture_profile("Staging", "开发环境", "198.51.100.8"),
        ],
    };
    let store_path = std::env::temp_dir()
        .join(format!("r-shell-workbench-{}", std::process::id()))
        .join("profiles.json");
    let state = AppState {
        window: window.clone(),
        notebook: notebook.clone(),
        workspace_stack: workspace.clone(),
        status,
        host_info,
        profiles_list: profiles.clone(),
        profiles_doc: Rc::new(RefCell::new(document)),
        profile_store: ProfileStore::new(&store_path),
        app_settings: Rc::new(RefCell::new(settings)),
        terminal_appearance: appearance.clone(),
        terminal_widgets: Rc::new(RefCell::new(Vec::new())),
        page_tabs: Rc::new(RefCell::new(Vec::new())),
        page_contexts: Rc::new(RefCell::new(Vec::new())),
        page_shutdowns: Rc::new(RefCell::new(Vec::new())),
        connections: Rc::new(RefCell::new(SessionHandles::default())),
        host_stats_poller: Rc::new(RefCell::new(None)),
        active_popover: Rc::new(RefCell::new(None)),
        local_terminals: Rc::new(Vec::new()),
        chrome: chrome.clone(),
        session_tab_strip: tabs,
        sftp_sidebar: files.clone(),
    };
    connect_workbench_actions(&state);
    reset_sftp_sidebar(&files, "/old", "Old host");
    let old_generation = files.connection_generation.get();
    reset_sftp_sidebar(&files, "/new", "New host");
    apply_sftp_connection_result(
        &state,
        &files,
        old_generation,
        "old".into(),
        "Old host ready",
        Err(anyhow::anyhow!("Late connection error")),
    );
    assert_eq!(files.path_label.text(), "/new");
    assert_eq!(files.status_label.text(), "New host");
    refresh_profile_list_for_state(&state);
    let header_row = profiles.row_at_index(0).unwrap();
    assert!(profile_for_row(&state.profiles_doc.borrow(), &header_row).is_none());
    chrome.profile_search.set_text("staging");
    let row = profiles.row_at_index(1).unwrap();
    let selected = profile_for_row(&state.profiles_doc.borrow(), &row).unwrap();
    assert_eq!(selected.host.as_deref(), Some("198.51.100.8"));
    assert!(profiles.row_at_index(2).is_none());
    chrome.profile_search.set_text("no-such-host");
    assert!(profiles.row_at_index(0).is_none());
    assert!(chrome.empty_profiles.get_visible());
    chrome.profile_search.set_text("");
    assert!(!chrome.empty_profiles.get_visible());
    profiles.select_row(profiles.row_at_index(1).as_ref());

    let (_, form) = build_new_session_form(&language);
    populate_form_from_profile(&form, &selected);
    assert_eq!(
        form.collect().unwrap().profile.group.as_deref(),
        Some("开发环境")
    );
    form.group.set_text("  新分组  ");
    assert_eq!(
        form.collect().unwrap().profile.group.as_deref(),
        Some("新分组")
    );
    form.group.set_text("  ");
    assert_eq!(form.collect().unwrap().profile.group, None);

    let terminal = GtkTerminalView::with_appearance(
        TerminalBuffer::new(TerminalSize::new(100, 32), 200),
        appearance,
    );
    let area = terminal.widget();
    notebook.append_page(&area, None::<&Widget>);
    register_page_tab(&state, &area, "Production API");
    for title in ["Staging", "PowerShell"] {
        let page = GtkBox::new(Orientation::Vertical, 0);
        notebook.append_page(&page, None::<&Widget>);
        register_page_tab(&state, &page, title);
    }
    notebook.set_current_page(Some(0));
    update_workbench_context(&state, notebook.nth_page(Some(2)).as_ref());
    assert_eq!(
        chrome.session_title.text(),
        "PowerShell",
        "use the page from switch-page, not the previous current_page"
    );
    workspace.set_visible_child_name(WORKSPACE_PAGE_SESSIONS);
    chrome.session_title.set_text("Production API");
    chrome.session_detail.set_text("deploy@192.0.2.10:22");
    chrome.protocol_badge.set_text("SSH");
    chrome.protocol_badge.set_visible(true);
    sync_session_tab_strip(&state, Some(area.upcast_ref()));
    window.present();
    settle_layout();
    let controllers = window.observe_controllers();
    let keys = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .find_map(|item| item.downcast::<gtk4::EventControllerKey>().ok())
        .unwrap();
    let shortcut = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
    assert!(!keys.emit_by_name::<bool>(
        "key-pressed",
        &[&gdk::Key::b, &0_u32, &gdk::ModifierType::CONTROL_MASK]
    ));
    assert!(keys.emit_by_name::<bool>("key-pressed", &[&gdk::Key::B, &0_u32, &shortcut]));
    assert!(!chrome.side_pane.start_child().unwrap().get_visible());
    assert!(keys.emit_by_name::<bool>("key-pressed", &[&gdk::Key::K, &0_u32, &shortcut]));
    assert!(chrome.side_pane.start_child().unwrap().get_visible());
    chrome.profile_search.select_region(0, 0);
    area.grab_focus();
    sync_session_tab_strip(&state, Some(area.upcast_ref()));
    settle_layout();
    terminal.feed(b"\x1b[38;2;117;184;158mdeploy@production-api\x1b[0m  \x1b[38;2;151;164;182m~/services\x1b[0m\r\n$ systemctl status api\r\n\r\n\x1b[32m  Active: active (running)\x1b[0m since Sun 2026-10-11 09:41:12 UTC\r\n  Main PID: 1842 (api-server)\r\n  Memory: 42.8M\r\n\r\n$ ls -lh\r\ntotal 48K\r\n\x1b[34mconfig/\x1b[0m     \x1b[34mlogs/\x1b[0m     docker-compose.yml     README.md\r\n\r\n$ ");
    chrome.files_button.emit_clicked();
    settle_layout();
    assert!(chrome.files_visible());
    assert!(files.container.allocated_height() >= 160);
    let files_height = files.container.allocated_height();
    chrome.focus_button.emit_clicked();
    settle_layout();
    assert!(!files.container.is_visible());
    assert!(!chrome.side_pane.start_child().unwrap().is_visible());
    assert!(chrome.layout_settings().files_visible);
    chrome.focus_button.emit_clicked();
    settle_layout();
    assert!(files.container.is_visible());
    assert!(chrome.side_pane.start_child().unwrap().is_visible());
    assert!((files.container.allocated_height() - files_height).abs() <= 4);
    files.path_label.set_text("/home/deploy/services");
    files.status_label.set_text("4 个项目 · 演示数据");
    for (name, directory, size) in [
        ("config", true, "4 KB"),
        ("logs", true, "4 KB"),
        ("docker-compose.yml", false, "1.8 KB"),
        ("README.md", false, "2.4 KB"),
    ] {
        append_sftp_row(
            &files.list,
            SftpRowDisplay {
                name,
                modified: "2026-10-11 09:41",
                permissions: if directory {
                    "drwxr-xr-x"
                } else {
                    "-rw-r--r--"
                },
                size,
                icon_name: if directory {
                    "folder-symbolic"
                } else {
                    "text-x-generic-symbolic"
                },
                row_class: if directory {
                    "sftp-dir-row"
                } else {
                    "sftp-file-row"
                },
                compact: false,
            },
        );
    }
    state.status.set_text("SSH · 演示会话");
    settle_layout();
    save_snapshot(&window, "workbench-1280");
    assert!(
        window.width() <= 1320,
        "header must not expand the window: {}",
        window.width()
    );
    // Remap to apply the requested size consistently on the Win32 backend.
    window.set_visible(false);
    window.set_default_size(960, 680);
    window.present();
    settle_layout();
    assert!(
        window.width() <= 1000,
        "window must remain resizable: {}",
        window.width()
    );
    assert!(area.allocated_width() > 600);
    save_snapshot(&window, "workbench-960");
    chrome.toggle_focus();
    settle_layout();
    save_snapshot(&window, "workbench-focus");
    let expected_layout = chrome.layout_settings();
    window.close();
    assert_eq!(
        state
            .profile_store
            .load_settings()
            .unwrap()
            .workspace_layout,
        expected_layout
    );
    window.destroy();
    fs::remove_file(store_path.parent().unwrap().join("settings.json")).unwrap();
    fs::remove_dir(store_path.parent().unwrap()).unwrap();
}
