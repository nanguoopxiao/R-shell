use super::*;
use shell_storage::WorkspaceLayoutSettings;

#[derive(Clone, Debug, PartialEq, Eq)]
struct PanelVisibility {
    sidebar: bool,
    files: bool,
    before_focus: Option<(bool, bool)>,
}

impl PanelVisibility {
    fn from_settings(settings: &WorkspaceLayoutSettings) -> Self {
        Self {
            sidebar: settings.sidebar_visible,
            files: settings.files_visible,
            before_focus: None,
        }
    }

    fn toggle_focus(&mut self) {
        if let Some((sidebar, files)) = self.before_focus.take() {
            self.sidebar = sidebar;
            self.files = files;
        } else {
            self.before_focus = Some((self.sidebar, self.files));
            self.sidebar = false;
            self.files = false;
        }
    }

    fn set_sidebar(&mut self, visible: bool) {
        self.leave_focus();
        self.sidebar = visible;
    }

    fn set_files(&mut self, visible: bool) {
        self.leave_focus();
        self.files = visible;
    }

    fn leave_focus(&mut self) {
        if self.before_focus.is_some() {
            self.toggle_focus();
        }
    }

    fn normal_visibility(&self) -> (bool, bool) {
        self.before_focus.unwrap_or((self.sidebar, self.files))
    }
}

pub(super) struct WorkbenchParts<'a> {
    pub tabs: &'a SessionTabStrip,
    pub profiles: &'a ListBox,
    pub files: &'a SftpSidebar,
    pub workspace: &'a Stack,
    pub status: &'a Label,
    pub host_info: &'a Label,
}

#[derive(Clone)]
pub(super) struct Workbench {
    pub header: HeaderBar,
    pub root: GtkBox,
    pub new_session_button: Button,
    pub settings_button: Button,
    pub sidebar_title: Label,
    pub profile_search: Entry,
    pub empty_profiles: Label,
    pub sidebar_button: Button,
    pub files_button: Button,
    pub focus_button: Button,
    pub close_files_button: Button,
    pub session_title: Label,
    pub session_detail: Label,
    pub protocol_badge: Label,
    pub side_pane: Paned,
    pub workspace_pane: Paned,
    sidebar: GtkBox,
    files: GtkBox,
    files_label: Label,
    language: Rc<RefCell<AppLanguage>>,
    visibility: Rc<RefCell<PanelVisibility>>,
    sidebar_width: Rc<Cell<i32>>,
    files_height: Rc<Cell<i32>>,
}

impl Workbench {
    pub fn new(
        language: &AppLanguage,
        parts: WorkbenchParts<'_>,
        settings: WorkspaceLayoutSettings,
    ) -> Self {
        let settings = settings.normalized();
        let sidebar_button = build_header_icon_button("sidebar-show-symbolic", "");
        let brand = GtkBox::new(Orientation::Horizontal, 8);
        brand.add_css_class("workbench-brand");
        brand.set_size_request(204, -1);
        let brand_name = Label::new(Some("R-shell"));
        brand_name.add_css_class("workbench-brand-name");
        brand.append(&sidebar_button);
        brand.append(&brand_name);

        let header = HeaderBar::new();
        header.add_css_class("app-header");
        header.pack_start(&brand);
        parts.tabs.container.set_hexpand(true);
        parts.tabs.container.set_halign(gtk4::Align::Fill);
        header.set_title_widget(Some(&parts.tabs.container));
        let settings_button = build_header_icon_button("preferences-system-symbolic", "");
        header.pack_end(&settings_button);

        let sidebar = GtkBox::new(Orientation::Vertical, 8);
        sidebar.add_css_class("workbench-sidebar");
        sidebar.set_size_request(180, -1);
        let sidebar_heading = GtkBox::new(Orientation::Horizontal, 4);
        sidebar_heading.add_css_class("workbench-sidebar-heading");
        let sidebar_title = Label::new(None);
        sidebar_title.set_hexpand(true);
        sidebar_title.set_xalign(0.0);
        sidebar_title.add_css_class("sidebar-panel-title");
        let new_session_button = build_header_icon_button("list-add-symbolic", "");
        sidebar_heading.append(&sidebar_title);
        sidebar_heading.append(&new_session_button);
        let profile_search = Entry::new();
        profile_search.set_icon_from_icon_name(
            gtk4::EntryIconPosition::Primary,
            Some("system-search-symbolic"),
        );
        profile_search.add_css_class("workbench-search");
        profile_search.set_hexpand(true);
        profile_search.set_width_chars(10);
        let scroll = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(PolicyType::Never)
            .child(parts.profiles)
            .build();
        scroll.add_css_class("sidebar-scroll");
        let empty_profiles = Label::new(None);
        empty_profiles.add_css_class("workbench-empty-profiles");
        empty_profiles.set_wrap(true);
        empty_profiles.set_xalign(0.0);
        sidebar.append(&sidebar_heading);
        sidebar.append(&profile_search);
        sidebar.append(&empty_profiles);
        sidebar.append(&scroll);

        let context = GtkBox::new(Orientation::Horizontal, 10);
        context.add_css_class("workbench-context");
        let protocol_badge = Label::new(None);
        protocol_badge.add_css_class("workbench-protocol");
        protocol_badge.set_visible(false);
        let session_title = Label::new(None);
        session_title.set_xalign(0.0);
        session_title.set_ellipsize(pango::EllipsizeMode::End);
        session_title.set_width_chars(1);
        session_title.add_css_class("workbench-session-name");
        let session_detail = Label::new(None);
        session_detail.set_hexpand(true);
        session_detail.set_xalign(0.0);
        session_detail.set_ellipsize(pango::EllipsizeMode::Middle);
        session_detail.set_width_chars(1);
        session_detail.add_css_class("workbench-session-detail");
        let files_button = Button::new();
        files_button.set_has_frame(false);
        files_button.add_css_class("workbench-action");
        let files_button_content = GtkBox::new(Orientation::Horizontal, 5);
        files_button_content.append(&Image::from_icon_name("folder-symbolic"));
        let files_label = Label::new(None);
        files_button_content.append(&files_label);
        files_button.set_child(Some(&files_button_content));
        let focus_button = build_header_icon_button("view-fullscreen-symbolic", "");
        context.append(&protocol_badge);
        context.append(&session_title);
        context.append(&session_detail);
        context.append(&files_button);
        context.append(&focus_button);

        parts.files.container.add_css_class("workbench-files");
        parts.files.container.set_visible(false);
        parts.files.scroll.set_min_content_height(72);
        let close_files_button = build_header_icon_button("pan-down-symbolic", "");
        parts.files.header_row.append(&close_files_button);
        let workspace_pane = Paned::new(Orientation::Vertical);
        workspace_pane.set_start_child(Some(parts.workspace));
        workspace_pane.set_end_child(Some(&parts.files.container));
        workspace_pane.set_resize_start_child(true);
        workspace_pane.set_resize_end_child(false);
        workspace_pane.set_shrink_start_child(false);
        workspace_pane.set_shrink_end_child(false);
        workspace_pane.set_vexpand(true);
        workspace_pane.set_wide_handle(false);
        parts.workspace.set_size_request(0, 150);
        let main = GtkBox::new(Orientation::Vertical, 0);
        main.add_css_class("content-panel");
        main.set_hexpand(true);
        main.append(&context);
        main.append(&workspace_pane);
        let side_pane = Paned::new(Orientation::Horizontal);
        side_pane.add_css_class("app-root");
        side_pane.set_start_child(Some(&sidebar));
        side_pane.set_end_child(Some(&main));
        side_pane.set_resize_start_child(false);
        side_pane.set_resize_end_child(true);
        side_pane.set_shrink_start_child(false);
        side_pane.set_shrink_end_child(false);
        side_pane.set_wide_handle(false);
        side_pane.set_position(settings.sidebar_width);
        side_pane.set_vexpand(true);

        let footer = GtkBox::new(Orientation::Horizontal, 16);
        footer.add_css_class("app-status-bar");
        parts.status.set_hexpand(true);
        parts.status.set_width_chars(1);
        parts.status.set_ellipsize(pango::EllipsizeMode::End);
        parts.host_info.set_hexpand(false);
        parts.host_info.set_max_width_chars(72);
        parts.host_info.set_ellipsize(pango::EllipsizeMode::End);
        for label in [parts.status, parts.host_info] {
            label.connect_label_notify(|label| label.set_tooltip_text(Some(label.text().as_str())));
        }
        footer.append(parts.status);
        footer.append(parts.host_info);
        let root = GtkBox::new(Orientation::Vertical, 0);
        root.append(&side_pane);
        root.append(&footer);

        let result = Self {
            header,
            root,
            new_session_button,
            settings_button,
            sidebar_title,
            profile_search,
            empty_profiles,
            sidebar_button,
            files_button,
            focus_button,
            close_files_button,
            session_title,
            session_detail,
            protocol_badge,
            side_pane,
            workspace_pane,
            sidebar,
            files: parts.files.container.clone(),
            files_label,
            language: Rc::new(RefCell::new(language.clone())),
            visibility: Rc::new(RefCell::new(PanelVisibility::from_settings(&settings))),
            sidebar_width: Rc::new(Cell::new(settings.sidebar_width)),
            files_height: Rc::new(Cell::new(settings.files_height)),
        };
        result.update_language(language);
        result.apply_visibility();
        result
    }

    pub fn update_language(&self, language: &AppLanguage) {
        *self.language.borrow_mut() = language.clone();
        self.sidebar_title
            .set_text(tr(language, "会话", "Sessions"));
        self.profile_search.set_placeholder_text(Some(tr(
            language,
            "搜索名称、地址或分组",
            "Search name, host or group",
        )));
        self.new_session_button.set_tooltip_text(Some(tr(
            language,
            "新建连接 (Ctrl+Shift+N)",
            "New connection (Ctrl+Shift+N)",
        )));
        self.settings_button
            .set_tooltip_text(Some(tr(language, "设置", "Settings")));
        self.files_label.set_text(tr(language, "文件", "Files"));
        self.files_button.set_tooltip_text(Some(tr(
            language,
            "远程文件面板 (Ctrl+Shift+E)",
            "Remote files (Ctrl+Shift+E)",
        )));
        self.close_files_button.set_tooltip_text(Some(tr(
            language,
            "收起文件面板",
            "Hide file panel",
        )));
        self.apply_visibility();
    }

    pub fn files_visible(&self) -> bool {
        self.visibility.borrow().files
    }

    pub fn set_files_visible(&self, visible: bool) {
        self.remember_sizes();
        self.visibility.borrow_mut().set_files(visible);
        self.apply_visibility();
    }

    pub fn set_sidebar_visible(&self, visible: bool) {
        self.remember_sizes();
        self.visibility.borrow_mut().set_sidebar(visible);
        self.apply_visibility();
    }

    pub fn toggle_sidebar(&self) {
        let visible = !self.visibility.borrow().sidebar;
        self.set_sidebar_visible(visible);
    }

    pub fn toggle_focus(&self) {
        self.remember_sizes();
        self.visibility.borrow_mut().toggle_focus();
        self.apply_visibility();
    }

    fn remember_sizes(&self) {
        if self.sidebar.is_visible() && self.side_pane.position() > 0 {
            self.sidebar_width
                .set(self.side_pane.position().clamp(180, 420));
        }
        if self.files.is_visible() && self.files.allocated_height() > 0 {
            self.files_height
                .set(self.files.allocated_height().clamp(160, 480));
        }
    }

    pub fn layout_settings(&self) -> WorkspaceLayoutSettings {
        self.remember_sizes();
        let (sidebar_visible, files_visible) = self.visibility.borrow().normal_visibility();
        WorkspaceLayoutSettings {
            sidebar_visible,
            files_visible,
            sidebar_width: self.sidebar_width.get(),
            files_height: self.files_height.get(),
        }
        .normalized()
    }

    fn apply_visibility(&self) {
        let visibility = self.visibility.borrow().clone();
        let showing_files = visibility.files && !self.files.get_visible();
        self.sidebar.set_visible(visibility.sidebar);
        self.files.set_visible(visibility.files);
        if visibility.sidebar {
            self.side_pane.set_position(self.sidebar_width.get());
        }
        set_button_selected(&self.sidebar_button, visibility.sidebar);
        set_button_selected(&self.files_button, visibility.files);
        set_button_selected(&self.focus_button, visibility.before_focus.is_some());
        let language = self.language.borrow();
        self.sidebar_button.set_tooltip_text(Some(tr(
            &language,
            "显示或隐藏会话 (Ctrl+Shift+B)",
            "Toggle sessions (Ctrl+Shift+B)",
        )));
        self.focus_button
            .set_tooltip_text(Some(if visibility.before_focus.is_some() {
                tr(
                    &language,
                    "退出专注模式 (Ctrl+Shift+F)",
                    "Leave focus mode (Ctrl+Shift+F)",
                )
            } else {
                tr(
                    &language,
                    "专注模式 (Ctrl+Shift+F)",
                    "Focus mode (Ctrl+Shift+F)",
                )
            }));
        self.focus_button.set_child(Some(&Image::from_icon_name(
            if visibility.before_focus.is_some() {
                "view-restore-symbolic"
            } else {
                "view-fullscreen-symbolic"
            },
        )));
        if showing_files {
            let height = Rc::clone(&self.files_height);
            self.workspace_pane.add_tick_callback(move |pane, _| {
                if !pane.end_child().is_some_and(|widget| widget.is_visible()) {
                    return ControlFlow::Break;
                }
                let available = pane.allocated_height();
                if available <= 0 {
                    return ControlFlow::Continue;
                }
                pane.set_position((available - height.get()).max(150));
                ControlFlow::Break
            });
        }
    }
}

fn set_button_selected(button: &Button, selected: bool) {
    if selected {
        button.add_css_class("active");
    } else {
        button.remove_css_class("active");
    }
}

pub(super) fn connect_workbench_actions(state: &AppState) {
    let cloned = state.clone();
    state
        .chrome
        .sidebar_button
        .connect_clicked(move |_| cloned.chrome.toggle_sidebar());
    let cloned = state.clone();
    state.chrome.focus_button.connect_clicked(move |_| {
        cloned.chrome.toggle_focus();
        if cloned.chrome.files_visible() {
            activate_sftp_sidebar_for_current_session(&cloned);
        }
    });
    let cloned = state.clone();
    state.chrome.files_button.connect_clicked(move |_| {
        let visible = !cloned.chrome.files_visible();
        cloned.chrome.set_files_visible(visible);
        if visible {
            activate_sftp_sidebar_for_current_session(&cloned);
        }
    });
    let cloned = state.clone();
    state
        .chrome
        .close_files_button
        .connect_clicked(move |_| cloned.chrome.set_files_visible(false));
    let cloned = state.clone();
    state
        .chrome
        .profile_search
        .connect_changed(move |_| refresh_profile_list_for_state(&cloned));
    let cloned = state.clone();
    state.window.connect_close_request(move |_| {
        let mut settings = cloned.app_settings.borrow().clone();
        settings.workspace_layout = cloned.chrome.layout_settings();
        if let Err(error) = cloned.profile_store.save_settings(&settings) {
            tracing::warn!("Could not save workspace layout: {error}");
        }
        glib::Propagation::Proceed
    });
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let cloned = state.clone();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.contains(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK)
            || modifiers.contains(gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::b | gdk::Key::B => cloned.chrome.sidebar_button.emit_clicked(),
            gdk::Key::e | gdk::Key::E => cloned.chrome.files_button.emit_clicked(),
            gdk::Key::f | gdk::Key::F => cloned.chrome.focus_button.emit_clicked(),
            gdk::Key::n | gdk::Key::N => cloned.chrome.new_session_button.emit_clicked(),
            gdk::Key::k | gdk::Key::K => {
                cloned.chrome.set_sidebar_visible(true);
                cloned.chrome.profile_search.grab_focus();
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    state.window.add_controller(keys);
}

pub(super) fn update_workbench_context(state: &AppState, active_page: Option<&Widget>) {
    let language = state.app_settings.borrow().language.clone();
    let page = active_page.cloned().or_else(|| {
        state
            .notebook
            .current_page()
            .and_then(|i| state.notebook.nth_page(Some(i)))
    });
    let context = page
        .as_ref()
        .and_then(|page| page_context_for_widget(state, page));
    if let Some(context) = context {
        let profile = &context.profile;
        state.chrome.session_title.set_text(&profile.name);
        let detail = match (&profile.host, profile.auth.username()) {
            (Some(host), Some(user)) => format!(
                "{user}@{host}:{}",
                profile.port.unwrap_or(default_port_for(&profile.protocol))
            ),
            (Some(host), None) => format!(
                "{host}:{}",
                profile.port.unwrap_or(default_port_for(&profile.protocol))
            ),
            _ => profile.serial_port.clone().unwrap_or_default(),
        };
        state.chrome.session_detail.set_text(&detail);
        state.chrome.session_detail.set_tooltip_text(Some(&detail));
        state
            .chrome
            .protocol_badge
            .set_text(protocol_label(&profile.protocol));
        state.chrome.protocol_badge.set_visible(true);
    } else {
        let title = page.and_then(|page| page_tab_title(state, &page));
        state
            .chrome
            .session_title
            .set_text(
                title
                    .as_deref()
                    .unwrap_or(tr(&language, "工作区", "Workspace")),
            );
        state.chrome.session_detail.set_text("");
        state.chrome.session_detail.set_tooltip_text(None);
        state.chrome.protocol_badge.set_visible(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_restores_each_previous_panel_combination() {
        for sidebar in [false, true] {
            for files in [false, true] {
                let mut state = PanelVisibility {
                    sidebar,
                    files,
                    before_focus: None,
                };
                state.toggle_focus();
                assert!(!state.sidebar && !state.files);
                assert_eq!(state.normal_visibility(), (sidebar, files));
                state.toggle_focus();
                assert_eq!((state.sidebar, state.files), (sidebar, files));
            }
        }
    }

    #[test]
    fn explicit_panel_action_leaves_focus_mode() {
        let mut state = PanelVisibility::from_settings(&WorkspaceLayoutSettings::default());
        state.toggle_focus();
        state.set_files(true);
        assert!(state.sidebar && state.files);
        assert!(state.before_focus.is_none());
    }
}
