use super::*;

const SFTP_PROGRESS_PULSE_INTERVAL_MS: u64 = 80;
const SFTP_PROGRESS_PULSE_STEP: f64 = 0.04;

pub(super) fn build_sftp_sidebar(compact: bool, language: &AppLanguage) -> SftpSidebar {
    let container = GtkBox::new(Orientation::Vertical, 8);
    container.add_css_class("sidebar-panel");

    let title_row = GtkBox::new(Orientation::Horizontal, 6);
    let title = Label::new(Some(tr(language, "SFTP 浏览器", "SFTP Browser")));
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.add_css_class("sidebar-panel-title");

    let search_entry = Entry::builder()
        .placeholder_text(tr(
            language,
            "搜索当前目录文件",
            "Search files in current directory",
        ))
        .hexpand(true)
        .build();
    search_entry.add_css_class("sftp-search");

    let path_label = Label::new(Some(tr(
        language,
        "选择一个 SSH 或 SFTP 会话。",
        "Select an SSH or SFTP session.",
    )));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.set_ellipsize(pango::EllipsizeMode::Middle);
    path_label.set_width_chars(1);
    path_label.set_max_width_chars(32);
    path_label.add_css_class("sftp-path");

    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.set_activate_on_single_click(false);
    list.add_css_class("profiles-list");
    list.add_css_class("sftp-list");

    let header = build_sftp_header_row(compact, language);

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&list)
        .build();
    scroll.add_css_class("sidebar-scroll");

    let scroll_for_wheel = scroll.clone();
    let wheel = EventControllerScroll::new(
        EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::DISCRETE,
    );
    wheel.connect_scroll(move |_, _dx, dy| {
        let adjustment = scroll_for_wheel.vadjustment();
        let step = adjustment.step_increment().max(48.0);
        let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        let value = (adjustment.value() + dy * step).clamp(adjustment.lower(), max_value);
        adjustment.set_value(value);
        glib::Propagation::Stop
    });
    scroll.add_controller(wheel);

    let status_label = Label::new(Some(tr(
        language,
        "点击 SFTP 图标浏览当前远程会话。",
        "Click the SFTP icon to browse the current remote session.",
    )));
    status_label.set_xalign(0.0);
    status_label.set_wrap(true);
    status_label.add_css_class("sftp-status");

    let progress_bar = ProgressBar::new();
    progress_bar.set_pulse_step(SFTP_PROGRESS_PULSE_STEP);
    progress_bar.set_show_text(true);
    progress_bar.set_text(Some(tr(language, "等待传输", "Transfer pending")));
    progress_bar.set_visible(false);

    let refresh_button = Button::new();
    refresh_button.set_child(Some(&Image::from_icon_name("view-refresh-symbolic")));
    refresh_button.set_has_frame(false);
    refresh_button.add_css_class("sftp-toolbar-button");
    refresh_button.set_tooltip_text(Some(tr(language, "刷新", "Refresh")));

    let upload_button = Button::new();
    upload_button.set_child(Some(&Image::from_icon_name("document-send-symbolic")));
    upload_button.set_has_frame(false);
    upload_button.add_css_class("sftp-toolbar-button");
    upload_button.set_tooltip_text(Some(tr(
        language,
        "使用系统文件选择器上传",
        "Upload using the system file picker",
    )));

    let download_button = Button::with_label(tr(language, "下载", "Download"));
    let mkdir_button = Button::with_label(tr(language, "新建", "New"));
    let rename_button = Button::with_label(tr(language, "重命名", "Rename"));
    let delete_button = Button::with_label(tr(language, "删除", "Delete"));
    download_button.set_tooltip_text(Some(tr(
        language,
        "下载选中的远程文件",
        "Download the selected remote file",
    )));
    mkdir_button.set_tooltip_text(Some(tr(language, "新建文件夹", "New folder")));
    rename_button.set_tooltip_text(Some(tr(
        language,
        "重命名选中的文件或文件夹",
        "Rename the selected file or folder",
    )));
    title_row.append(&title);
    title_row.append(&refresh_button);
    title_row.append(&upload_button);

    container.append(&title_row);
    container.append(&search_entry);
    container.append(&path_label);
    container.append(&header);
    container.append(&scroll);
    container.append(&status_label);
    container.append(&progress_bar);

    SftpSidebar {
        container,
        compact,
        title_label: title,
        search_entry,
        path_label,
        list,
        scroll,
        status_label,
        progress_bar,
        refresh_button,
        upload_button,
        download_button,
        mkdir_button,
        rename_button,
        delete_button,
        session: Rc::new(RefCell::new(None)),
        config_key: Rc::new(RefCell::new(None)),
        clipboard: Rc::new(RefCell::new(None)),
    }
}

pub(super) fn build_sftp_header_row(compact: bool, language: &AppLanguage) -> GtkBox {
    let row = GtkBox::new(Orientation::Horizontal, 8);
    row.add_css_class("sftp-header-row");
    row.set_margin_start(6);
    row.set_margin_end(6);

    if compact {
        let name = sftp_column_label(tr(language, "名称", "Name"), 96, true);
        let details = sftp_column_label(tr(language, "详情", "Details"), 96, false);
        row.append(&name);
        row.append(&details);
        return row;
    }

    let name = sftp_column_label(tr(language, "名称", "Name"), 220, true);
    let modified = sftp_column_label(tr(language, "修改时间", "Modified"), 150, false);
    let permissions = sftp_column_label(tr(language, "权限", "Permissions"), 100, false);
    let size = sftp_column_label(tr(language, "大小", "Size"), 80, false);
    row.append(&name);
    row.append(&modified);
    row.append(&permissions);
    row.append(&size);
    row
}

pub(super) fn sftp_column_label(text: &str, width: i32, expand: bool) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_width_chars(1);
    label.set_size_request(width, -1);
    label.set_hexpand(expand);
    label.add_css_class("sftp-header-label");
    label
}

pub(super) fn clear_list_box(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

pub(super) fn reset_sftp_sidebar(sidebar: &SftpSidebar, path: &str, status: &str) {
    clear_list_box(&sidebar.list);
    *sidebar.session.borrow_mut() = None;
    *sidebar.config_key.borrow_mut() = None;
    sidebar.search_entry.set_text("");
    sidebar.path_label.set_text(path);
    sidebar.status_label.set_text(status);
    sidebar.scroll.vadjustment().set_value(0.0);
}

pub(super) fn refresh_sftp_list(sidebar: &SftpSidebar) {
    refresh_sftp_list_internal(sidebar, true);
}

pub(super) fn refresh_sftp_list_from_top(sidebar: &SftpSidebar) {
    refresh_sftp_list_internal(sidebar, false);
}

pub(super) fn refresh_sftp_list_internal(sidebar: &SftpSidebar, preserve_scroll: bool) {
    let filter_text = sidebar.search_entry.text().to_string();
    let adjustment = sidebar.scroll.vadjustment();
    let previous_value = if preserve_scroll {
        adjustment.value()
    } else {
        0.0
    };
    populate_sftp_list_filtered(
        &sidebar.list,
        &sidebar.session,
        &sidebar.status_label,
        &filter_text,
        sidebar.compact,
    );
    let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(previous_value.clamp(adjustment.lower(), max_value));
}

pub(super) fn sftp_row_name(row: &ListBoxRow) -> Option<String> {
    row.tooltip_text().map(|text| text.to_string())
}

pub(super) fn sftp_row_is_parent(row: &ListBoxRow) -> bool {
    row.has_css_class("sftp-up-row")
}

pub(super) fn sftp_row_is_directory(row: &ListBoxRow) -> bool {
    row.has_css_class("sftp-dir-row") || row.has_css_class("sftp-up-row")
}

pub(super) fn sftp_row_is_file(row: &ListBoxRow) -> bool {
    row.has_css_class("sftp-file-row")
}

pub(super) fn begin_sftp_progress(browser: &SftpSidebar, text: &str) -> glib::SourceId {
    browser.progress_bar.set_fraction(0.0);
    browser.progress_bar.set_text(Some(text));
    browser.progress_bar.set_visible(true);
    let progress_bar = browser.progress_bar.clone();
    glib::timeout_add_local(
        Duration::from_millis(SFTP_PROGRESS_PULSE_INTERVAL_MS),
        move || {
            progress_bar.pulse();
            ControlFlow::Continue
        },
    )
}

pub(super) fn finish_sftp_progress(browser: &SftpSidebar, source_id: glib::SourceId) {
    source_id.remove();
    browser.progress_bar.set_fraction(0.0);
    browser.progress_bar.set_visible(false);
}

pub(super) fn connect_sftp_sidebar_actions(state: &AppState) {
    connect_sftp_browser_actions(state, &state.sftp_sidebar);
}

pub(super) fn connect_sftp_browser_actions(state: &AppState, browser: &SftpSidebar) {
    let browser_for_search = browser.clone();
    browser_for_search
        .search_entry
        .clone()
        .connect_changed(move |_| {
            refresh_sftp_list_from_top(&browser_for_search);
        });

    let browser_for_refresh = browser.clone();
    let refresh_button = browser_for_refresh.refresh_button.clone();
    refresh_button.connect_clicked(move |_| {
        refresh_sftp_list(&browser_for_refresh);
    });

    let browser_for_nav = browser.clone();
    let nav_list = browser_for_nav.list.clone();
    nav_list.connect_row_activated(move |_, row| {
        let Some(name) = sftp_row_name(row) else {
            return;
        };
        let Some(session) = browser_for_nav.session.borrow().as_ref().cloned() else {
            return;
        };

        let result = if sftp_row_is_parent(row) {
            session.cd("..")
        } else if sftp_row_is_directory(row) {
            session.cd(&name)
        } else {
            return;
        };

        match result {
            Ok(()) => match session.cwd() {
                Ok(cwd) => {
                    browser_for_nav.path_label.set_text(&cwd.to_string_lossy());
                    refresh_sftp_list_from_top(&browser_for_nav);
                }
                Err(err) => browser_for_nav
                    .status_label
                    .set_text(&format!("Open folder failed: {err}")),
            },
            Err(err) => browser_for_nav
                .status_label
                .set_text(&format!("Open folder failed: {err}")),
        }
    });

    let browser_for_download = browser.clone();
    let state_for_download = state.clone();
    let download_button = browser_for_download.download_button.clone();
    download_button.connect_clicked(move |_| {
        let Some(row) = browser_for_download.list.selected_row() else {
            browser_for_download
                .status_label
                .set_text("Select a remote file first");
            return;
        };
        if sftp_row_is_parent(&row) {
            return;
        }
        let is_dir = sftp_row_is_directory(&row);
        if !is_dir && !sftp_row_is_file(&row) {
            browser_for_download
                .status_label
                .set_text("Select a file or folder to download");
            return;
        }
        let Some(name) = sftp_row_name(&row) else {
            return;
        };
        let browser_for_path = browser_for_download.clone();
        let state_for_path = state_for_download.clone();
        if is_dir {
            show_select_folder_dialog(
                &state_for_download.window,
                "Download SFTP Folder",
                "Download",
                move |local_path| {
                    let Some(session) = browser_for_path.session.borrow().as_ref().cloned() else {
                        browser_for_path
                            .status_label
                            .set_text("SFTP session is not connected");
                        return;
                    };
                    let name_for_task = name.clone();
                    let name_for_result = name.clone();
                    let browser_for_result = browser_for_path.clone();
                    let progress_id = begin_sftp_progress(&browser_for_result, "Downloading…");
                    let pending = format!("Downloading {name_for_result}...");
                    run_background_task(
                        &state_for_path,
                        Some(&pending),
                        move || session.download_recursive(&name_for_task, &local_path),
                        move |state, result| match result {
                            Ok(bytes) => {
                                finish_sftp_progress(&browser_for_result, progress_id);
                                browser_for_result.status_label.set_text(&format!(
                                    "Downloaded {name_for_result} ({})",
                                    format_bytes(bytes)
                                ));
                                state.status.set_text("SFTP download complete");
                            }
                            Err(err) => {
                                finish_sftp_progress(&browser_for_result, progress_id);
                                browser_for_result
                                    .status_label
                                    .set_text(&format!("Download failed: {err}"));
                                state
                                    .status
                                    .set_text(&format!("SFTP download failed: {err}"));
                            }
                        },
                    );
                },
            );
        } else {
            let suggested_name = name.clone();
            show_save_file_dialog(
                &state_for_download.window,
                "Download SFTP File",
                &suggested_name,
                move |local_path| {
                    let Some(session) = browser_for_path.session.borrow().as_ref().cloned() else {
                        browser_for_path
                            .status_label
                            .set_text("SFTP session is not connected");
                        return;
                    };
                    let name_for_task = name.clone();
                    let name_for_result = name.clone();
                    let browser_for_result = browser_for_path.clone();
                    let progress_id = begin_sftp_progress(&browser_for_result, "Downloading…");
                    let pending = format!("Downloading {name_for_result}...");
                    run_background_task(
                        &state_for_path,
                        Some(&pending),
                        move || session.download_resume(&name_for_task, &local_path),
                        move |state, result| match result {
                            Ok(bytes) => {
                                finish_sftp_progress(&browser_for_result, progress_id);
                                browser_for_result.status_label.set_text(&format!(
                                    "Downloaded {name_for_result} ({})",
                                    format_bytes(bytes)
                                ));
                                state.status.set_text("SFTP download complete");
                            }
                            Err(err) => {
                                finish_sftp_progress(&browser_for_result, progress_id);
                                browser_for_result
                                    .status_label
                                    .set_text(&format!("Download failed: {err}"));
                                state
                                    .status
                                    .set_text(&format!("SFTP download failed: {err}"));
                            }
                        },
                    );
                },
            );
        }
    });

    let browser_for_upload = browser.clone();
    let state_for_upload = state.clone();
    let upload_button = browser_for_upload.upload_button.clone();
    upload_button.connect_clicked(move |_| {
        let browser_for_path = browser_for_upload.clone();
        let state_for_path = state_for_upload.clone();
        show_open_files_dialog(
            &state_for_upload.window,
            "Upload SFTP File",
            "Upload",
            move |local_paths| {
                upload_sftp_paths(&state_for_path, &browser_for_path, local_paths);
            },
        );
    });

    let browser_for_rename = browser.clone();
    let state_for_rename = state.clone();
    let rename_button = browser_for_rename.rename_button.clone();
    rename_button.connect_clicked(move |_| {
        let Some(row) = browser_for_rename.list.selected_row() else {
            browser_for_rename
                .status_label
                .set_text("Select an item to rename");
            return;
        };
        if sftp_row_is_parent(&row) {
            return;
        }
        let Some(old_name) = sftp_row_name(&row) else {
            return;
        };
        let initial_name = old_name.clone();
        let browser_for_prompt = browser_for_rename.clone();
        let state_for_prompt = state_for_rename.clone();
        let language = state_for_rename.app_settings.borrow().language.clone();
        show_text_prompt(
            &state_for_rename.window,
            tr(&language, "重命名 SFTP 项目", "Rename SFTP Item"),
            tr(&language, "新的远程名称", "New remote name"),
            &initial_name,
            tr(&language, "重命名", "Rename"),
            tr(&language, "取消", "Cancel"),
            move |new_name| {
                let new_name = new_name.trim().to_string();
                if new_name.is_empty() || new_name == old_name {
                    return;
                }
                let Some(session) = browser_for_prompt.session.borrow().as_ref().cloned() else {
                    browser_for_prompt
                        .status_label
                        .set_text("SFTP session is not connected");
                    return;
                };
                let old_name_for_task = old_name.clone();
                let new_name_for_task = new_name.clone();
                let browser_for_result = browser_for_prompt.clone();
                let pending = format!("Renaming {old_name}...");
                run_background_task(
                    &state_for_prompt,
                    Some(&pending),
                    move || session.rename(&old_name_for_task, &new_name_for_task),
                    move |state, result| match result {
                        Ok(()) => {
                            refresh_sftp_list(&browser_for_result);
                            browser_for_result.status_label.set_text("Rename complete");
                            state.status.set_text("SFTP rename complete");
                        }
                        Err(err) => {
                            browser_for_result
                                .status_label
                                .set_text(&format!("Rename failed: {err}"));
                            state.status.set_text(&format!("SFTP rename failed: {err}"));
                        }
                    },
                );
            },
        );
    });

    let browser_for_delete = browser.clone();
    let state_for_delete = state.clone();
    let delete_button = browser_for_delete.delete_button.clone();
    delete_button.connect_clicked(move |_| {
        let Some(row) = browser_for_delete.list.selected_row() else {
            browser_for_delete
                .status_label
                .set_text("Select an item to delete");
            return;
        };
        let Some(name) = sftp_row_name(&row) else {
            return;
        };
        if sftp_row_is_parent(&row) {
            return;
        }
        let Some(session) = browser_for_delete.session.borrow().as_ref().cloned() else {
            browser_for_delete
                .status_label
                .set_text("SFTP session is not connected");
            return;
        };
        let is_dir = sftp_row_is_directory(&row);
        let name_for_task = name.clone();
        let name_for_result = name.clone();
        let browser_for_result = browser_for_delete.clone();
        let pending = format!("Deleting {name_for_result}...");
        run_background_task(
            &state_for_delete,
            Some(&pending),
            move || session.delete_entry(&name_for_task, is_dir),
            move |state, result| match result {
                Ok(()) => {
                    refresh_sftp_list(&browser_for_result);
                    browser_for_result.status_label.set_text("Delete complete");
                    state.status.set_text("SFTP delete complete");
                }
                Err(err) => {
                    browser_for_result
                        .status_label
                        .set_text(&format!("Delete failed: {err}"));
                    state.status.set_text(&format!("SFTP delete failed: {err}"));
                }
            },
        );
    });

    let browser_for_mkdir = browser.clone();
    let parent_window = state.window.clone();
    let state_for_mkdir = state.clone();
    let mkdir_button = browser_for_mkdir.mkdir_button.clone();
    mkdir_button.connect_clicked(move |_| {
        let browser_for_prompt = browser_for_mkdir.clone();
        let state_for_prompt = state_for_mkdir.clone();
        let language = state_for_mkdir.app_settings.borrow().language.clone();
        show_text_prompt(
            &parent_window,
            tr(&language, "新建文件夹", "New Folder"),
            tr(&language, "远程文件夹名称", "Remote folder name"),
            "",
            tr(&language, "创建", "Create"),
            tr(&language, "取消", "Cancel"),
            move |name| {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return;
                }
                let Some(session) = browser_for_prompt.session.borrow().as_ref().cloned() else {
                    browser_for_prompt
                        .status_label
                        .set_text("SFTP session is not connected");
                    return;
                };
                let name_for_task = name.clone();
                let browser_for_result = browser_for_prompt.clone();
                let pending = format!("Creating {name}...");
                run_background_task(
                    &state_for_prompt,
                    Some(&pending),
                    move || session.mkdir(&name_for_task),
                    move |state, result| match result {
                        Ok(()) => {
                            refresh_sftp_list(&browser_for_result);
                            browser_for_result.status_label.set_text("Folder created");
                            state.status.set_text("SFTP folder created");
                        }
                        Err(err) => {
                            browser_for_result
                                .status_label
                                .set_text(&format!("mkdir failed: {err}"));
                            state.status.set_text(&format!("SFTP mkdir failed: {err}"));
                        }
                    },
                );
            },
        );
    });

    connect_sftp_context_menu(state, browser);
    connect_sftp_drop_upload(state, browser);
}

pub(super) fn connect_sftp_drop_upload(state: &AppState, browser: &SftpSidebar) {
    let drop_target = DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
    let state_for_drop = state.clone();
    let browser_for_drop = browser.clone();
    drop_target.connect_drop(move |_, value, _x, _y| {
        let Ok(file) = value.get::<gio::File>() else {
            browser_for_drop
                .status_label
                .set_text("Drop from the system file manager to upload a file.");
            return false;
        };
        let Some(path) = file.path() else {
            browser_for_drop
                .status_label
                .set_text("Dropped item has no local filesystem path");
            return false;
        };
        upload_sftp_paths(&state_for_drop, &browser_for_drop, vec![path]);
        true
    });
    browser.list.add_controller(drop_target);
}

pub(super) fn connect_sftp_context_menu(state: &AppState, browser: &SftpSidebar) {
    let click = GestureClick::new();
    click.set_button(3);
    let state_for_click = state.clone();
    let browser_for_click = browser.clone();
    click.connect_released(move |_, _presses, x, y| {
        let row = browser_for_click.list.row_at_y(y as i32);
        if let Some(row) = row.as_ref() {
            browser_for_click.list.select_row(Some(row));
        }
        show_sftp_actions_popover(
            &state_for_click,
            &browser_for_click,
            row,
            gdk::Rectangle::new(x as i32, y as i32, 240, 20),
        );
    });
    browser.list.add_controller(click);
}

pub(super) fn show_sftp_actions_popover(
    state: &AppState,
    browser: &SftpSidebar,
    row: Option<ListBoxRow>,
    anchor: gdk::Rectangle,
) {
    let popover = Popover::new();
    popover.set_has_arrow(false);
    popover.set_position(PositionType::Bottom);
    popover.set_offset(8, 8);
    popover.set_parent(&browser.list);
    popover.set_pointing_to(Some(&anchor));

    let root = GtkBox::new(Orientation::Vertical, 2);
    root.add_css_class("context-menu-list");

    let has_session = browser.session.borrow().is_some();
    let has_entry = row.as_ref().is_some_and(|row| !sftp_row_is_parent(row));
    let paste_available = sftp_paste_available(browser);
    let language = state.app_settings.borrow().language.clone();

    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "上传文件…", "Upload File…"),
        has_session,
        {
            let upload_button = browser.upload_button.clone();
            move || upload_button.emit_clicked()
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "新建文件…", "New File…"),
        has_session,
        {
            let state = state.clone();
            let browser = browser.clone();
            move || create_sftp_file_prompt(&state, &browser)
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "新建文件夹…", "New Folder…"),
        has_session,
        {
            let mkdir_button = browser.mkdir_button.clone();
            move || mkdir_button.emit_clicked()
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "重命名…", "Rename…"),
        has_entry,
        {
            let rename_button = browser.rename_button.clone();
            move || rename_button.emit_clicked()
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "下载…", "Download…"),
        has_entry,
        {
            let download_button = browser.download_button.clone();
            move || download_button.emit_clicked()
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "复制", "Copy"),
        has_entry,
        {
            let browser = browser.clone();
            move || copy_sftp_selection(&browser)
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "粘贴", "Paste"),
        paste_available,
        {
            let state = state.clone();
            let browser = browser.clone();
            move || paste_sftp_clipboard(&state, &browser)
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "压缩为 .tar.gz", "Compress .tar.gz"),
        has_entry,
        {
            let state = state.clone();
            let browser = browser.clone();
            move || compress_sftp_selection(&state, &browser)
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "修改权限…", "Permissions…"),
        has_entry,
        {
            let state = state.clone();
            let browser = browser.clone();
            move || chmod_sftp_selection_prompt(&state, &browser)
        },
    );
    append_sftp_action_button(
        &root,
        &popover,
        tr(&language, "删除", "Delete"),
        has_entry,
        {
            let delete_button = browser.delete_button.clone();
            move || delete_button.emit_clicked()
        },
    );

    popover.set_child(Some(&root));
    register_active_popover(state, &popover);
    popover.popup();
}

pub(super) fn append_sftp_action_button(
    root: &GtkBox,
    popover: &Popover,
    label: &str,
    enabled: bool,
    action: impl Fn() + 'static,
) {
    let button = Button::with_label(label);
    button.add_css_class("profile-actions-button");
    button.add_css_class("context-menu-button");
    button.set_sensitive(enabled);
    let popover = popover.clone();
    button.connect_clicked(move |_| {
        popover.popdown();
        action();
    });
    root.append(&button);
}

pub(super) fn create_sftp_file_prompt(state: &AppState, browser: &SftpSidebar) {
    let browser_for_prompt = browser.clone();
    let state_for_prompt = state.clone();
    let language = state.app_settings.borrow().language.clone();
    show_text_prompt(
        &state.window,
        tr(&language, "新建文件", "New File"),
        tr(&language, "远程文件名", "Remote file name"),
        "",
        tr(&language, "创建", "Create"),
        tr(&language, "取消", "Cancel"),
        move |name| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let Some(session) = browser_for_prompt.session.borrow().as_ref().cloned() else {
                browser_for_prompt
                    .status_label
                    .set_text("SFTP session is not connected");
                return;
            };
            let name_for_task = name.clone();
            let browser_for_result = browser_for_prompt.clone();
            let pending = format!("Creating file {name}...");
            run_background_task(
                &state_for_prompt,
                Some(&pending),
                move || session.create_file(&name_for_task),
                move |state, result| match result {
                    Ok(()) => {
                        refresh_sftp_list(&browser_for_result);
                        browser_for_result.status_label.set_text("File created");
                        state.status.set_text("SFTP file created");
                    }
                    Err(err) => {
                        browser_for_result
                            .status_label
                            .set_text(&format!("Create file failed: {err}"));
                        state
                            .status
                            .set_text(&format!("SFTP create file failed: {err}"));
                    }
                },
            );
        },
    );
}

pub(super) fn upload_sftp_paths(
    state: &AppState,
    browser: &SftpSidebar,
    local_paths: Vec<PathBuf>,
) {
    if local_paths.is_empty() {
        return;
    }
    let Some(session) = browser.session.borrow().as_ref().cloned() else {
        browser
            .status_label
            .set_text("SFTP session is not connected");
        return;
    };

    let upload_count = local_paths.len();
    let browser_for_result = browser.clone();
    let progress_id = begin_sftp_progress(&browser_for_result, "Uploading…");
    let pending = if upload_count == 1 {
        format!("Uploading {}...", local_paths[0].display())
    } else {
        format!("Uploading {upload_count} files...")
    };
    run_background_task(
        state,
        Some(&pending),
        move || {
            let mut total = 0u64;
            for local_path in local_paths {
                let Some(remote_name) = local_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .filter(|name| !name.is_empty())
                else {
                    anyhow::bail!("Upload path has no file name: {}", local_path.display())
                };
                total += session.upload_recursive(&local_path, &remote_name)?;
            }
            Ok(total)
        },
        move |state, result| match result {
            Ok(bytes) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                refresh_sftp_list(&browser_for_result);
                browser_for_result.status_label.set_text(&format!(
                    "Uploaded {upload_count} item(s), {}",
                    format_bytes(bytes)
                ));
                state.status.set_text("SFTP upload complete");
            }
            Err(err) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                browser_for_result
                    .status_label
                    .set_text(&format!("Upload failed: {err}"));
                state.status.set_text(&format!("SFTP upload failed: {err}"));
            }
        },
    );
}

pub(super) fn sftp_paste_available(browser: &SftpSidebar) -> bool {
    let Some(clipboard) = browser.clipboard.borrow().as_ref().cloned() else {
        return false;
    };
    browser.session.borrow().is_some()
        && browser
            .config_key
            .borrow()
            .as_deref()
            .is_some_and(|key| key == clipboard.config_key)
}

pub(super) fn copy_sftp_selection(browser: &SftpSidebar) {
    let Some(row) = browser.list.selected_row() else {
        browser.status_label.set_text("Select an item to copy");
        return;
    };
    if sftp_row_is_parent(&row) {
        return;
    }
    let Some(name) = sftp_row_name(&row) else {
        return;
    };
    let Some(session) = browser.session.borrow().as_ref().cloned() else {
        browser
            .status_label
            .set_text("SFTP session is not connected");
        return;
    };
    let Some(config_key) = browser.config_key.borrow().clone() else {
        browser
            .status_label
            .set_text("SFTP session is not connected");
        return;
    };
    let source_path = match session.absolute_path(&name) {
        Ok(path) => path.to_string_lossy().replace('\\', "/"),
        Err(err) => {
            browser
                .status_label
                .set_text(&format!("Copy failed: {err}"));
            return;
        }
    };
    let is_dir = sftp_row_is_directory(&row);
    *browser.clipboard.borrow_mut() = Some(SftpClipboard {
        config_key,
        name: name.clone(),
        source_path,
        is_dir,
    });
    browser
        .status_label
        .set_text(&format!("Copied {name}. Right-click this folder to paste."));
}

pub(super) fn paste_sftp_clipboard(state: &AppState, browser: &SftpSidebar) {
    let Some(clipboard) = browser.clipboard.borrow().as_ref().cloned() else {
        browser.status_label.set_text("Nothing copied yet");
        return;
    };
    if !sftp_paste_available(browser) {
        browser
            .status_label
            .set_text("Copied item belongs to a different SFTP session");
        return;
    }
    let Some(session) = browser.session.borrow().as_ref().cloned() else {
        browser
            .status_label
            .set_text("SFTP session is not connected");
        return;
    };

    let destination_name = pasted_name_for(&clipboard.name, clipboard.is_dir);
    let source_path = clipboard.source_path.clone();
    let destination_for_result = destination_name.clone();
    let browser_for_result = browser.clone();
    let progress_id = begin_sftp_progress(&browser_for_result, "Copying…");
    let pending = format!("Copying {}...", clipboard.name);
    run_background_task(
        state,
        Some(&pending),
        move || session.copy_entry(&source_path, &destination_name),
        move |state, result| match result {
            Ok(bytes) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                refresh_sftp_list(&browser_for_result);
                browser_for_result.status_label.set_text(&format!(
                    "Pasted {destination_for_result} ({})",
                    format_bytes(bytes)
                ));
                state.status.set_text("SFTP paste complete");
            }
            Err(err) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                browser_for_result
                    .status_label
                    .set_text(&format!("Paste failed: {err}"));
                state.status.set_text(&format!("SFTP paste failed: {err}"));
            }
        },
    );
}

pub(super) fn pasted_name_for(name: &str, is_dir: bool) -> String {
    if is_dir {
        return format!("{name} copy");
    }
    if let Some((stem, extension)) = name.rsplit_once('.')
        && !stem.is_empty()
        && !extension.is_empty()
    {
        return format!("{stem} copy.{extension}");
    }
    format!("{name} copy")
}

pub(super) fn compress_sftp_selection(state: &AppState, browser: &SftpSidebar) {
    let Some(row) = browser.list.selected_row() else {
        browser.status_label.set_text("Select an item to compress");
        return;
    };
    if sftp_row_is_parent(&row) {
        return;
    }
    let Some(name) = sftp_row_name(&row) else {
        return;
    };
    let Some(session) = browser.session.borrow().as_ref().cloned() else {
        browser
            .status_label
            .set_text("SFTP session is not connected");
        return;
    };
    let name_for_task = name.clone();
    let browser_for_result = browser.clone();
    let progress_id = begin_sftp_progress(&browser_for_result, "Compressing…");
    let pending = format!("Compressing {name}...");
    run_background_task(
        state,
        Some(&pending),
        move || session.compress_tar_gz(&name_for_task),
        move |state, result| match result {
            Ok(archive_name) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                refresh_sftp_list(&browser_for_result);
                browser_for_result
                    .status_label
                    .set_text(&format!("Created {archive_name}"));
                state.status.set_text("SFTP compression complete");
            }
            Err(err) => {
                finish_sftp_progress(&browser_for_result, progress_id);
                browser_for_result
                    .status_label
                    .set_text(&format!("Compress failed: {err}"));
                state
                    .status
                    .set_text(&format!("SFTP compression failed: {err}"));
            }
        },
    );
}

pub(super) fn chmod_sftp_selection_prompt(state: &AppState, browser: &SftpSidebar) {
    let Some(row) = browser.list.selected_row() else {
        browser.status_label.set_text("Select an item to chmod");
        return;
    };
    if sftp_row_is_parent(&row) {
        return;
    }
    let Some(name) = sftp_row_name(&row) else {
        return;
    };
    let initial_mode = if sftp_row_is_directory(&row) {
        "755"
    } else {
        "644"
    };
    let browser_for_prompt = browser.clone();
    let state_for_prompt = state.clone();
    let language = state.app_settings.borrow().language.clone();
    show_text_prompt(
        &state.window,
        tr(&language, "修改权限", "Change Permissions"),
        tr(
            &language,
            "八进制权限，例如 644 或 755",
            "Octal mode, for example 644 or 755",
        ),
        initial_mode,
        tr(&language, "应用", "Apply"),
        tr(&language, "取消", "Cancel"),
        move |mode_text| {
            let mode = match parse_octal_permissions(&mode_text) {
                Ok(mode) => mode,
                Err(err) => {
                    browser_for_prompt
                        .status_label
                        .set_text(&format!("Invalid mode: {err}"));
                    return;
                }
            };
            let Some(session) = browser_for_prompt.session.borrow().as_ref().cloned() else {
                browser_for_prompt
                    .status_label
                    .set_text("SFTP session is not connected");
                return;
            };
            let name_for_task = name.clone();
            let browser_for_result = browser_for_prompt.clone();
            let pending = format!("Changing permissions on {name}...");
            run_background_task(
                &state_for_prompt,
                Some(&pending),
                move || session.chmod(&name_for_task, mode),
                move |state, result| match result {
                    Ok(()) => {
                        refresh_sftp_list(&browser_for_result);
                        browser_for_result
                            .status_label
                            .set_text("Permissions updated");
                        state.status.set_text("SFTP permissions updated");
                    }
                    Err(err) => {
                        browser_for_result
                            .status_label
                            .set_text(&format!("chmod failed: {err}"));
                        state.status.set_text(&format!("SFTP chmod failed: {err}"));
                    }
                },
            );
        },
    );
}

pub(super) fn parse_octal_permissions(value: &str) -> anyhow::Result<u32> {
    let trimmed = value.trim().trim_start_matches("0o");
    if trimmed.is_empty() || trimmed.len() > 4 || !trimmed.chars().all(|ch| matches!(ch, '0'..='7'))
    {
        anyhow::bail!("expected an octal mode like 644 or 0755")
    }
    let mode = u32::from_str_radix(trimmed, 8)?;
    if mode > 0o7777 {
        anyhow::bail!("mode is out of range")
    }
    Ok(mode)
}

pub(super) fn build_sftp_config_from_profile(
    profile: &ConnectionProfile,
    password: Option<String>,
) -> anyhow::Result<SftpConfig> {
    if !matches!(profile.protocol, ProtocolKind::Ssh | ProtocolKind::Sftp) {
        anyhow::bail!("Select an SSH or SFTP session first")
    }

    if matches!(profile.auth, AuthConfig::KeyboardInteractive { .. }) {
        anyhow::bail!(
            "Keyboard-interactive SSH sessions cannot be reused by the embedded SFTP browser yet"
        )
    }

    if matches!(profile.auth, AuthConfig::Password { .. }) && password.is_none() {
        anyhow::bail!(
            "The current session does not have a reusable password. Reconnect with a saved password or open a saved SFTP profile."
        )
    }

    let host = profile
        .host
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Remote host is missing"))?;
    let mut config = SftpConfig::new(host, profile.auth.clone());
    config.port = profile.port.unwrap_or(22);
    config.password = password;
    Ok(config)
}

pub(super) fn sftp_target_key(config: &SftpConfig) -> String {
    format!(
        "{}:{}:{}:{:?}",
        config.host,
        config.port,
        config.auth.username().unwrap_or_default(),
        config.auth
    )
}

pub(super) fn open_sftp_browser_for_config(
    state: &AppState,
    browser: SftpSidebar,
    config: SftpConfig,
    ready_status: &'static str,
) {
    let target_key = sftp_target_key(&config);
    {
        let current_key = browser.config_key.borrow();
        let current_session = browser.session.borrow();
        if current_session.is_some() && current_key.as_deref() == Some(target_key.as_str()) {
            refresh_sftp_list(&browser);
            return;
        }
    }

    reset_sftp_sidebar(
        &browser,
        "/",
        &format!("Connecting to {}:{}…", config.host, config.port),
    );
    let browser_for_result = browser.clone();
    let status_message = format!("Connecting SFTP browser to {}…", config.host);
    let target_key_for_result = target_key.clone();
    run_background_task(
        state,
        Some(&status_message),
        move || SftpSession::connect(&config),
        move |state, result| match result {
            Ok(session) => {
                let cwd = match session.cwd() {
                    Ok(cwd) => cwd,
                    Err(err) => {
                        reset_sftp_sidebar(
                            &browser_for_result,
                            "/",
                            &format!("SFTP connection failed: {err}"),
                        );
                        state
                            .status
                            .set_text(&format!("SFTP connection failed: {err}"));
                        return;
                    }
                };
                *browser_for_result.session.borrow_mut() = Some(session);
                *browser_for_result.config_key.borrow_mut() = Some(target_key_for_result);
                browser_for_result
                    .path_label
                    .set_text(&cwd.to_string_lossy());
                browser_for_result.search_entry.set_text("");
                refresh_sftp_list(&browser_for_result);
                state.status.set_text(ready_status);
            }
            Err(err) => {
                reset_sftp_sidebar(
                    &browser_for_result,
                    "/",
                    &format!("SFTP connection failed: {err}"),
                );
                state
                    .status
                    .set_text(&format!("SFTP connection failed: {err}"));
            }
        },
    );
}

pub(super) fn open_sftp_sidebar_for_config(state: &AppState, config: SftpConfig) {
    open_sftp_browser_for_config(
        state,
        state.sftp_sidebar.clone(),
        config,
        "SFTP browser ready",
    );
}

pub(super) fn open_sftp_profile_tab(
    state: &AppState,
    profile: ConnectionProfile,
    password: Option<String>,
) -> anyhow::Result<()> {
    let config = build_sftp_config_from_profile(&profile, password.clone())?;
    let tab_title = format!("sftp {}", config.host);
    let browser = build_sftp_sidebar(false, &state.app_settings.borrow().language);
    browser
        .status_label
        .set_text(&format!("Connecting to {}:{}…", config.host, config.port));
    browser
        .path_label
        .set_text(&format!("{}:{}", config.host, config.port));
    connect_sftp_browser_actions(state, &browser);

    let tab_label = make_tab_label(&tab_title, &state.notebook, &browser.container);
    state
        .notebook
        .append_page(&browser.container, Some(&tab_label));
    register_page_context(state, &browser.container, profile.clone(), password.clone());
    register_page_tab(state, &browser.container, tab_title.clone());
    activate_session_page(state, browser.container.upcast_ref());

    request_host_stats(
        state,
        profile.host.clone().unwrap_or_default(),
        profile.port.unwrap_or(22),
        profile.auth.username().map(str::to_string),
        password,
    );
    open_sftp_browser_for_config(state, browser, config, "SFTP session ready");
    Ok(())
}

pub(super) fn activate_sftp_sidebar_for_current_session(state: &AppState) {
    activate_sftp_sidebar_for_page_context(state, current_page_context(state));
}

pub(super) fn activate_sftp_sidebar_for_page_context(
    state: &AppState,
    context: Option<PageContext>,
) {
    let Some(context) = context else {
        reset_sftp_sidebar(
            &state.sftp_sidebar,
            "/",
            "Select an active SSH or SFTP tab first.",
        );
        return;
    };

    match build_sftp_config_from_profile(&context.profile, context.runtime_password.clone()) {
        Ok(config) => open_sftp_sidebar_for_config(state, config),
        Err(err) => reset_sftp_sidebar(&state.sftp_sidebar, "/", &err.to_string()),
    }
}

#[allow(dead_code)]
pub(super) fn show_sftp_window(parent: &ApplicationWindow, config: SftpConfig) {
    let win = Window::builder()
        .title(format!("SFTP — {}", config.host))
        .transient_for(parent)
        .modal(false)
        .default_width(700)
        .default_height(500)
        .build();

    let vbox = GtkBox::new(Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);

    // 路径栏。
    let path_label = Label::new(Some("/"));
    path_label.set_xalign(0.0);
    vbox.append(&path_label);

    // 文件列表。
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    let scroll = ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&list)
        .build();
    vbox.append(&scroll);

    // 状态栏。
    let status_label = Label::new(Some("Connecting…"));
    status_label.set_xalign(0.0);
    vbox.append(&status_label);

    // 按钮栏。
    let btn_bar = GtkBox::new(Orientation::Horizontal, 6);
    let refresh_btn = Button::with_label("Refresh");
    let mkdir_btn = Button::with_label("New Folder");
    let delete_btn = Button::with_label("Delete");
    btn_bar.append(&refresh_btn);
    btn_bar.append(&mkdir_btn);
    btn_bar.append(&delete_btn);
    vbox.append(&btn_bar);

    win.set_child(Some(&vbox));
    win.present();

    // 在后台线程连接，随后回到主线程填充列表。
    let config_clone = config.clone();
    let session_rc: Rc<RefCell<Option<SftpSession>>> = Rc::new(RefCell::new(None));
    let session_for_list = Rc::clone(&session_rc);
    let list_for_populate = list.clone();
    let path_label_clone = path_label.clone();
    let status_clone = status_label.clone();

    // 通过通道把连接结果交回主线程。
    let (tx, rx) = mpsc::channel::<Result<SftpSession, String>>();
    std::thread::spawn(move || {
        let result = SftpSession::connect(&config_clone).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });

    // 在主线程轮询连接结果。
    glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
        Ok(Ok(session)) => {
            let cwd = match session.cwd() {
                Ok(cwd) => cwd,
                Err(err) => {
                    status_clone.set_text(&format!("Connection failed: {err}"));
                    return ControlFlow::Break;
                }
            };
            *session_for_list.borrow_mut() = Some(session);
            path_label_clone.set_text(&cwd.to_string_lossy());
            populate_sftp_list(&list_for_populate, &session_for_list, &status_clone);
            ControlFlow::Break
        }
        Ok(Err(err)) => {
            status_clone.set_text(&format!("Connection failed: {err}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status_clone.set_text("Connection thread crashed");
            ControlFlow::Break
        }
    });

    // 刷新按钮。
    let session_for_refresh = Rc::clone(&session_rc);
    let list_for_refresh = list.clone();
    let status_for_refresh = status_label.clone();
    refresh_btn.connect_clicked(move |_| {
        populate_sftp_list(&list_for_refresh, &session_for_refresh, &status_for_refresh);
    });

    // 双击进入目录。
    let session_for_nav = Rc::clone(&session_rc);
    let list_for_nav = list.clone();
    let path_for_nav = path_label.clone();
    let status_for_nav = status_label.clone();
    list.connect_row_activated(move |_, row| {
        let Some(name) = sftp_row_name(row) else {
            return;
        };
        if sftp_row_is_parent(row) {
            if let Some(session) = session_for_nav.borrow().as_ref() {
                match session.cd("..") {
                    Ok(()) => match session.cwd() {
                        Ok(cwd) => {
                            path_for_nav.set_text(&cwd.to_string_lossy());
                            populate_sftp_list(&list_for_nav, &session_for_nav, &status_for_nav);
                        }
                        Err(err) => status_for_nav.set_text(&format!("Open folder failed: {err}")),
                    },
                    Err(err) => status_for_nav.set_text(&format!("Open folder failed: {err}")),
                }
            }
        } else if sftp_row_is_directory(row)
            && let Some(session) = session_for_nav.borrow().as_ref()
        {
            match session.cd(&name) {
                Ok(()) => match session.cwd() {
                    Ok(cwd) => {
                        path_for_nav.set_text(&cwd.to_string_lossy());
                        populate_sftp_list(&list_for_nav, &session_for_nav, &status_for_nav);
                    }
                    Err(err) => status_for_nav.set_text(&format!("Open folder failed: {err}")),
                },
                Err(err) => status_for_nav.set_text(&format!("Open folder failed: {err}")),
            }
        }
    });

    // 删除选中的远程条目。
    let session_for_del = Rc::clone(&session_rc);
    let list_for_del = list.clone();
    let status_for_del = status_label.clone();
    delete_btn.connect_clicked(move |_| {
        if let Some(row) = list_for_del.selected_row() {
            let Some(name) = sftp_row_name(&row) else {
                return;
            };
            if !sftp_row_is_parent(&row)
                && let Some(session) = session_for_del.borrow().as_ref()
            {
                match session.delete(&name) {
                    Ok(()) => populate_sftp_list(&list_for_del, &session_for_del, &status_for_del),
                    Err(err) => status_for_del.set_text(&format!("Delete failed: {err}")),
                }
            }
        }
    });

    // 新建文件夹：通过小对话框里的输入框获取名称。
    let session_for_mkdir = Rc::clone(&session_rc);
    let list_for_mkdir = list.clone();
    let status_for_mkdir = status_label.clone();
    let win_for_mkdir = win.clone();
    mkdir_btn.connect_clicked(move |_| {
        let dialog = build_modal_window(&win_for_mkdir, "New Folder", 300, 0);
        let dbox = GtkBox::new(Orientation::Vertical, 8);
        dbox.set_margin_top(12);
        dbox.set_margin_bottom(12);
        dbox.set_margin_start(12);
        dbox.set_margin_end(12);
        let entry = Entry::builder().placeholder_text("folder name").build();
        let ok_btn = Button::with_label("Create");
        dbox.append(&entry);
        dbox.append(&ok_btn);
        dialog.set_child(Some(&dbox));

        let session_inner = Rc::clone(&session_for_mkdir);
        let list_inner = list_for_mkdir.clone();
        let status_inner = status_for_mkdir.clone();
        let dialog_clone = dialog.clone();
        ok_btn.connect_clicked(move |_| {
            let name = entry.text().trim().to_string();
            if !name.is_empty()
                && let Some(session) = session_inner.borrow().as_ref()
            {
                match session.mkdir(&name) {
                    Ok(()) => populate_sftp_list(&list_inner, &session_inner, &status_inner),
                    Err(err) => status_inner.set_text(&format!("mkdir failed: {err}")),
                }
            }
            dialog_clone.close();
        });
        dialog.present();
    });
}

pub(super) fn populate_sftp_list(
    list: &ListBox,
    session: &Rc<RefCell<Option<SftpSession>>>,
    status: &Label,
) {
    populate_sftp_list_filtered(list, session, status, "", false);
}

struct SftpRowDisplay<'a> {
    name: &'a str,
    modified: &'a str,
    permissions: &'a str,
    size: &'a str,
    icon_name: &'a str,
    row_class: &'a str,
    compact: bool,
}

fn append_sftp_row(list: &ListBox, display: SftpRowDisplay<'_>) {
    let row = ListBoxRow::new();
    row.add_css_class("sftp-entry-row");
    row.add_css_class(display.row_class);
    row.set_tooltip_text(Some(display.name));

    if display.compact {
        row.add_css_class("sftp-entry-row-compact");

        let content = GtkBox::new(Orientation::Horizontal, 8);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(6);
        content.set_margin_end(6);

        let icon = Image::from_icon_name(display.icon_name);
        icon.set_icon_size(gtk4::IconSize::Normal);
        icon.add_css_class("sftp-entry-icon");

        let text_column = GtkBox::new(Orientation::Vertical, 2);
        text_column.set_hexpand(true);
        text_column.set_size_request(0, -1);

        let name_label = Label::new(Some(display.name));
        name_label.set_xalign(0.0);
        name_label.set_ellipsize(pango::EllipsizeMode::Middle);
        name_label.add_css_class("sftp-entry-name");

        let detail_text = compact_sftp_detail(display.modified, display.permissions, display.size);
        let detail_label = Label::new(Some(&detail_text));
        detail_label.set_xalign(0.0);
        detail_label.set_ellipsize(pango::EllipsizeMode::End);
        detail_label.add_css_class("sftp-entry-detail");
        detail_label.add_css_class("sftp-metadata-column");

        text_column.append(&name_label);
        text_column.append(&detail_label);
        content.append(&icon);
        content.append(&text_column);
        row.set_child(Some(&content));
        list.append(&row);
        return;
    }

    let content = GtkBox::new(Orientation::Horizontal, 8);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);

    let icon = Image::from_icon_name(display.icon_name);
    icon.set_icon_size(gtk4::IconSize::Normal);
    icon.add_css_class("sftp-entry-icon");

    let name_column = GtkBox::new(Orientation::Horizontal, 8);
    name_column.set_hexpand(true);
    name_column.set_size_request(220, -1);

    let name_label = Label::new(Some(display.name));
    name_label.set_xalign(0.0);
    name_label.set_ellipsize(pango::EllipsizeMode::Middle);
    name_label.add_css_class("sftp-entry-name");
    name_label.add_css_class("sftp-name-column");

    let modified_label = sftp_metadata_label(display.modified, 150);
    let permissions_label = sftp_metadata_label(display.permissions, 100);
    let size_label = sftp_metadata_label(display.size, 80);

    content.append(&icon);
    name_column.append(&name_label);
    content.append(&name_column);
    content.append(&modified_label);
    content.append(&permissions_label);
    content.append(&size_label);
    row.set_child(Some(&content));
    list.append(&row);
}

pub(super) fn compact_sftp_detail(modified: &str, permissions: &str, size: &str) -> String {
    [modified, permissions, size]
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ")
}

pub(super) fn sftp_metadata_label(text: &str, width: i32) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_width_chars(1);
    label.set_size_request(width, -1);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.add_css_class("sftp-metadata-column");
    label
}

pub(super) fn populate_sftp_list_filtered(
    list: &ListBox,
    session: &Rc<RefCell<Option<SftpSession>>>,
    status: &Label,
    filter_text: &str,
    compact: bool,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    let borrowed = session.borrow();
    let Some(sftp) = borrowed.as_ref() else {
        return;
    };

    match sftp.list() {
        Ok(entries) => {
            let normalized_filter = filter_text.trim().to_lowercase();
            append_sftp_row(
                list,
                SftpRowDisplay {
                    name: "..",
                    modified: "Parent directory",
                    permissions: "",
                    size: "",
                    icon_name: "go-up-symbolic",
                    row_class: "sftp-up-row",
                    compact,
                },
            );

            let mut visible_entries = 0usize;
            for entry in &entries {
                if !normalized_filter.is_empty()
                    && !entry.name.to_lowercase().contains(&normalized_filter)
                {
                    continue;
                }

                let permissions = format_permissions(entry.permissions, entry.is_dir);
                let modified = format_unix_timestamp(entry.modified);
                let size = format_bytes(entry.size);
                let icon_name = if entry.is_dir {
                    "folder-symbolic"
                } else {
                    "text-x-generic-symbolic"
                };
                let row_class = if entry.is_dir {
                    "sftp-dir-row"
                } else {
                    "sftp-file-row"
                };
                append_sftp_row(
                    list,
                    SftpRowDisplay {
                        name: &entry.name,
                        modified: &modified,
                        permissions: &permissions,
                        size: &size,
                        icon_name,
                        row_class,
                        compact,
                    },
                );
                visible_entries += 1;
            }

            if normalized_filter.is_empty() {
                status.set_text(&format!("{} items", entries.len()));
            } else {
                status.set_text(&format!("{} / {} items", visible_entries, entries.len()));
            }
        }
        Err(err) => {
            status.set_text(&format!("List failed: {err}"));
        }
    }
}
