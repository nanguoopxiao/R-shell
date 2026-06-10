use super::*;

pub(super) const SESSION_TAB_MAX_WIDTH: i32 = 220;
pub(super) const SESSION_TAB_MIN_WIDTH: i32 = 184;
pub(super) const SESSION_TAB_CLOSE_WIDTH: i32 = 24;
pub(super) const SESSION_TAB_LABEL_MIN_WIDTH: i32 = 128;
pub(super) const SESSION_TAB_INNER_CHROME_WIDTH: i32 = 28;
pub(super) const SESSION_TAB_CONTROL_GAP: i32 = 8;
pub(super) const SESSION_TAB_SPACING: i32 = 6;
pub(super) const SESSION_TAB_STRIP_HORIZONTAL_PADDING: i32 = 16;
pub(super) const SESSION_TAB_BOX_LEADING_PADDING: i32 = 4;
pub(super) const SESSION_TAB_REVEAL_PADDING: i32 = 10;

pub(super) fn build_session_tab_strip(language: &AppLanguage) -> SessionTabStrip {
    let container = GtkBox::new(Orientation::Horizontal, SESSION_TAB_CONTROL_GAP);
    container.set_hexpand(true);
    container.set_size_request(0, -1);
    container.add_css_class("session-tab-strip");

    let controls = GtkBox::new(Orientation::Horizontal, 0);
    controls.set_hexpand(false);
    controls.add_css_class("session-tab-controls");

    let add_button = Button::new();
    add_button.set_tooltip_text(Some(tr(
        language,
        "打开首选本地终端",
        "Open the preferred local terminal",
    )));
    add_button.set_has_frame(false);
    add_button.add_css_class("session-strip-button");
    add_button.set_child(Some(&Image::from_icon_name("list-add-symbolic")));

    let dropdown_button = Button::new();
    dropdown_button.set_tooltip_text(Some(tr(
        language,
        "选择本地终端",
        "Choose a local terminal",
    )));
    dropdown_button.set_has_frame(false);
    dropdown_button.add_css_class("session-strip-button");
    dropdown_button.set_child(Some(&Image::from_icon_name("pan-down-symbolic")));

    controls.append(&add_button);
    controls.append(&dropdown_button);

    let tabs_box = GtkBox::new(Orientation::Horizontal, SESSION_TAB_SPACING);
    tabs_box.set_hexpand(false);
    tabs_box.set_halign(gtk4::Align::Start);
    tabs_box.add_css_class("session-tabs-box");

    let tabs_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::External)
        .vscrollbar_policy(PolicyType::Never)
        .hexpand(false)
        .child(&tabs_box)
        .build();
    tabs_scroll.add_css_class("session-tabs-scroll");
    tabs_scroll.set_size_request(0, -1);
    tabs_scroll.set_min_content_width(0);
    tabs_scroll.set_propagate_natural_width(false);
    tabs_scroll.set_propagate_natural_height(false);

    let tabs_scroll_for_wheel = tabs_scroll.clone();
    let wheel = EventControllerScroll::new(
        EventControllerScrollFlags::VERTICAL | EventControllerScrollFlags::DISCRETE,
    );
    wheel.connect_scroll(move |_, _dx, dy| {
        let adjustment = tabs_scroll_for_wheel.hadjustment();
        let step = adjustment.step_increment().max(64.0);
        let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        let value = (adjustment.value() + dy * step).clamp(adjustment.lower(), max_value);
        adjustment.set_value(value);
        glib::Propagation::Stop
    });
    tabs_scroll.add_controller(wheel);

    container.append(&tabs_scroll);
    container.append(&controls);

    SessionTabStrip {
        container,
        controls,
        tabs_scroll,
        tabs_box,
        add_button,
        dropdown_button,
        width_cap: Rc::new(RefCell::new(None)),
    }
}

pub(super) fn register_page_tab(
    state: &AppState,
    widget: &impl gtk4::prelude::IsA<Widget>,
    title: impl Into<String>,
) {
    let title = title.into();
    let weak = glib::WeakRef::<Widget>::new();
    weak.set(Some(widget.upcast_ref()));
    let widget_ptr = widget.upcast_ref::<Widget>().as_ptr();

    let mut page_tabs = state.page_tabs.borrow_mut();
    page_tabs.retain(|tab| {
        tab.widget
            .upgrade()
            .is_some_and(|tab_widget| state.notebook.page_num(&tab_widget).is_some())
    });

    if let Some(existing) = page_tabs.iter_mut().find(|tab| {
        tab.widget
            .upgrade()
            .is_some_and(|tab_widget| tab_widget.as_ptr() == widget_ptr)
    }) {
        existing.title = title;
        return;
    }

    page_tabs.push(PageTab {
        widget: weak,
        title,
    });
}

pub(super) fn prune_page_tabs(state: &AppState) {
    state.page_tabs.borrow_mut().retain(|tab| {
        tab.widget
            .upgrade()
            .is_some_and(|tab_widget| state.notebook.page_num(&tab_widget).is_some())
    });
}

pub(super) fn page_tab_title(state: &AppState, widget: &Widget) -> Option<String> {
    state.page_tabs.borrow().iter().find_map(|tab| {
        let tab_widget = tab.widget.upgrade()?;
        (tab_widget.as_ptr() == widget.as_ptr()).then_some(tab.title.clone())
    })
}

pub(super) fn sync_session_tab_strip(state: &AppState, active_page: Option<&Widget>) {
    // 根据 notebook 当前页面重建标签栏。点击/关闭回调使用页面 widget，而不是捕获的
    // 下标，因为任意页签删除后后续页面下标都会变化。
    let adjustment = state.session_tab_strip.tabs_scroll.hadjustment();
    let previous_scroll = adjustment.value();
    let tab_count = state.notebook.n_pages() as i32;
    // 只有显式激活/切换页面时才 reveal。关闭页签时传入 `None`，保持滚动偏移，
    // 让多页签横向溢出时用户可以连续点击同一关闭按钮位置。
    let reveal_active_page = active_page.is_some();
    let current_page_index = active_page
        .and_then(|page| state.notebook.page_num(page))
        .or_else(|| state.notebook.current_page());
    let controls_width = session_tab_controls_width(state);
    let available_tabs_width = session_tab_available_width(
        state.session_tab_strip.container.allocated_width(),
        controls_width,
    );
    let target_tab_width = session_tab_target_width(available_tabs_width, tab_count);
    let target_button_width = session_tab_button_width(target_tab_width);
    let target_label_chars = session_tab_label_max_chars(target_button_width);
    let compact_tabs = session_tab_is_compact(target_tab_width);
    let language = state.app_settings.borrow().language.clone();

    while let Some(child) = state.session_tab_strip.tabs_box.first_child() {
        state.session_tab_strip.tabs_box.remove(&child);
    }

    let current_page_ptr = active_page.map(|page| page.as_ptr() as usize).or_else(|| {
        state
            .notebook
            .current_page()
            .and_then(|index| state.notebook.nth_page(Some(index)))
            .map(|page| page.as_ptr() as usize)
    });

    for index in 0..state.notebook.n_pages() {
        let Some(page) = state.notebook.nth_page(Some(index)) else {
            continue;
        };
        let title =
            page_tab_title(state, &page).unwrap_or_else(|| format!("Session {}", index + 1));

        let item = GtkBox::new(Orientation::Horizontal, if compact_tabs { 2 } else { 4 });
        item.add_css_class("session-tab-item");
        if compact_tabs {
            item.add_css_class("compact");
        }
        item.set_hexpand(false);
        item.set_halign(gtk4::Align::Start);
        item.set_size_request(target_tab_width, -1);
        let is_active = current_page_ptr == Some(page.as_ptr() as usize);
        if is_active {
            item.add_css_class("active");
        }

        let tab_button = Button::new();
        tab_button.set_has_frame(false);
        tab_button.set_hexpand(true);
        tab_button.add_css_class("session-tab-button");
        tab_button.set_tooltip_text(Some(&title));
        tab_button.set_size_request(target_button_width, -1);

        let label = Label::new(Some(&title));
        label.set_ellipsize(pango::EllipsizeMode::Middle);
        label.set_max_width_chars(target_label_chars);
        label.set_width_chars(1);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        tab_button.set_child(Some(&label));

        let notebook_for_tab = state.notebook.clone();
        let page_for_tab = page.clone();
        tab_button.connect_clicked(move |_| {
            if let Some(page_num) = notebook_for_tab.page_num(&page_for_tab) {
                notebook_for_tab.set_current_page(Some(page_num));
            }
        });

        let close_button = Button::new();
        close_button.set_has_frame(false);
        close_button.set_hexpand(false);
        close_button.set_focusable(false);
        close_button.set_tooltip_text(Some(tr(&language, "关闭标签页", "Close tab")));
        close_button.add_css_class("tab-close-button");
        close_button.set_size_request(SESSION_TAB_CLOSE_WIDTH, -1);
        let close_icon = Image::from_icon_name("window-close-symbolic");
        close_icon.set_pixel_size(13);
        close_button.set_child(Some(&close_icon));
        let notebook_for_close = state.notebook.clone();
        let page_for_close = page.clone();
        close_button.connect_clicked(move |_| {
            if let Some(page_num) = notebook_for_close.page_num(&page_for_close) {
                notebook_for_close.remove_page(Some(page_num));
            }
        });

        item.append(&tab_button);
        item.append(&close_button);
        state.session_tab_strip.tabs_box.append(&item);
    }

    let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
    adjustment.set_value(previous_scroll.clamp(adjustment.lower(), max_value));

    sync_session_tab_strip_layout(state);
    if reveal_active_page {
        schedule_session_tab_reveal(state, current_page_index, tab_count, target_tab_width);
    }
}

pub(super) fn schedule_session_tab_strip_resync(state: &AppState) {
    let state_for_resync = state.clone();
    glib::idle_add_local_once(move || {
        sync_session_tab_strip(&state_for_resync, None);
    });
}

pub(super) fn animate_notebook_switch(notebook: &Notebook) {
    notebook.add_css_class("page-switching");
    let notebook = notebook.clone();
    glib::timeout_add_local(
        Duration::from_millis(PAGE_SWITCH_TRANSITION_MS),
        move || {
            notebook.remove_css_class("page-switching");
            ControlFlow::Break
        },
    );
}

pub(super) fn session_tab_controls_width(state: &AppState) -> i32 {
    let (_, controls_width, _, _) = state
        .session_tab_strip
        .controls
        .measure(Orientation::Horizontal, -1);
    controls_width
}

pub(super) fn session_tab_available_width(strip_width: i32, controls_width: i32) -> i32 {
    (strip_width.max(0)
        - controls_width.max(0)
        - SESSION_TAB_CONTROL_GAP
        - SESSION_TAB_STRIP_HORIZONTAL_PADDING
        - SESSION_TAB_BOX_LEADING_PADDING)
        .max(0)
}

pub(super) fn session_tab_target_width(available_tabs_width: i32, tab_count: i32) -> i32 {
    if tab_count <= 0 {
        return SESSION_TAB_MAX_WIDTH;
    }

    let gap_total = SESSION_TAB_SPACING * tab_count.saturating_sub(1);
    let width_budget = (available_tabs_width - gap_total).max(0);
    // 低于可读最小宽度时，优先让标签栏横向滚动，而不是继续压缩到协议/主机信息消失。
    (width_budget / tab_count).clamp(SESSION_TAB_MIN_WIDTH, SESSION_TAB_MAX_WIDTH)
}

pub(super) fn session_tab_effective_available_width(
    strip_available_tabs_width: i32,
    _current_scroll_width: i32,
) -> i32 {
    strip_available_tabs_width.max(0)
}

pub(super) fn sync_session_tab_strip_layout(state: &AppState) {
    let current_strip_width = state.session_tab_strip.container.allocated_width();
    if state.notebook.n_pages() == 0 && current_strip_width > 0 {
        *state.session_tab_strip.width_cap.borrow_mut() = Some(current_strip_width);
        capture_window_size_cap(state);
    }

    let tab_count = state.notebook.n_pages() as i32;
    let controls_width = session_tab_controls_width(state);
    let current_scroll_width = state.session_tab_strip.tabs_scroll.allocated_width();
    let strip_available_tabs_width =
        session_tab_available_width(current_strip_width, controls_width);
    // 判断是否溢出时使用页签条当前真实宽度预算。这里如果复用上一次滚动区分配，
    // 新增页签会短暂被误判为溢出，导致控制按钮被推到最右侧，直到 idle 重同步
    // 才恢复正常。
    let available_tabs_width =
        session_tab_effective_available_width(strip_available_tabs_width, current_scroll_width);
    let target_tab_width = session_tab_target_width(available_tabs_width, tab_count);
    let tabs_width = session_tab_measured_content_width(
        state,
        session_tab_content_width(target_tab_width, tab_count),
    );
    let tabs_overflow = tabs_width > available_tabs_width;
    let scroll_width = tabs_width.min(available_tabs_width).max(0);

    if std::env::var_os("SHELL_APP_DEBUG_TRACE_TAB_STRIP").is_some() {
        eprintln!(
            "tab-strip count={tab_count} strip={current_strip_width} scroll_alloc={current_scroll_width} controls={controls_width} strip_available={strip_available_tabs_width} available={available_tabs_width} target={target_tab_width} tabs={tabs_width} overflow={tabs_overflow} scroll={scroll_width}"
        );
    }

    state.session_tab_strip.container.set_size_request(0, -1);
    state
        .session_tab_strip
        .tabs_scroll
        .set_visible(tab_count > 0);
    state
        .session_tab_strip
        .tabs_scroll
        .set_hexpand(tabs_overflow);
    state
        .session_tab_strip
        .tabs_scroll
        .set_size_request(scroll_width, -1);

    clamp_window_size_to_cap(state);
}

pub(super) fn schedule_session_tab_reveal(
    state: &AppState,
    active_index: Option<u32>,
    tab_count: i32,
    target_tab_width: i32,
) {
    let Some(active_index) = active_index else {
        return;
    };
    if tab_count <= 0 || active_index >= tab_count as u32 {
        return;
    }

    let tabs_scroll = state.session_tab_strip.tabs_scroll.clone();
    glib::idle_add_local_once(move || {
        let adjustment = tabs_scroll.hadjustment();
        let page_size = adjustment.page_size();
        let max_value = (adjustment.upper() - page_size).max(adjustment.lower());
        if max_value <= adjustment.lower() || page_size <= 0.0 {
            return;
        }

        let tab_stride = target_tab_width + SESSION_TAB_SPACING;
        let tab_start =
            f64::from(SESSION_TAB_BOX_LEADING_PADDING + active_index as i32 * tab_stride);
        let tab_end = tab_start + f64::from(target_tab_width + SESSION_TAB_REVEAL_PADDING);
        let current_value = adjustment.value();
        let next_value = if tab_end > current_value + page_size {
            tab_end - page_size
        } else if tab_start < current_value {
            tab_start
        } else {
            current_value
        };

        adjustment.set_value(next_value.clamp(adjustment.lower(), max_value));
    });
}

pub(super) fn session_tab_measured_content_width(state: &AppState, fallback_width: i32) -> i32 {
    let (_, natural_width, _, _) = state
        .session_tab_strip
        .tabs_box
        .measure(Orientation::Horizontal, -1);
    natural_width.max(fallback_width)
}

pub(super) fn session_tab_content_width(target_tab_width: i32, tab_count: i32) -> i32 {
    if tab_count <= 0 {
        return 0;
    }

    target_tab_width * tab_count + SESSION_TAB_SPACING * tab_count.saturating_sub(1)
}

pub(super) fn session_tab_button_width(target_tab_width: i32) -> i32 {
    (target_tab_width - SESSION_TAB_CLOSE_WIDTH - SESSION_TAB_INNER_CHROME_WIDTH)
        .max(SESSION_TAB_LABEL_MIN_WIDTH)
}

pub(super) fn session_tab_label_max_chars(target_button_width: i32) -> i32 {
    (target_button_width / 8).clamp(10, 28)
}

pub(super) fn session_tab_is_compact(target_tab_width: i32) -> bool {
    target_tab_width <= 72
}

pub(super) fn clamp_window_size_to_cap(state: &AppState) {
    let Some((cap_width, cap_height)) = *state.window_size_cap.borrow() else {
        return;
    };

    let current_width = state.window.allocated_width();
    let current_height = state.window.allocated_height();
    if current_width <= cap_width && current_height <= cap_height {
        return;
    }

    state.window.set_default_size(cap_width, cap_height);
}

pub(super) fn activate_session_page(state: &AppState, widget: &Widget) {
    if let Some(page_num) = state.notebook.page_num(widget) {
        state.notebook.set_current_page(Some(page_num));
    }
    sync_workspace_state(state);
    sync_session_tab_strip(state, Some(widget));
    schedule_session_tab_strip_resync(state);
    schedule_widget_focus(widget);
}

pub(super) fn schedule_widget_focus(widget: &Widget) {
    let _ = widget.grab_focus();

    let widget = widget.clone();
    glib::idle_add_local_once(move || {
        if widget.root().is_some() && widget.is_visible() {
            let _ = widget.grab_focus();
        }
    });
}

pub(super) fn make_tab_label(
    title: &str,
    notebook: &Notebook,
    page_widget: &impl gtk4::prelude::IsA<Widget>,
) -> GtkBox {
    let hbox = GtkBox::new(Orientation::Horizontal, 4);
    let lbl = Label::new(Some(title));
    let close_btn = Button::new();
    close_btn.set_has_frame(false);
    close_btn.add_css_class("flat");
    close_btn.add_css_class("tab-close-button");
    let close_icon = Image::from_icon_name("window-close-symbolic");
    close_icon.set_pixel_size(13);
    close_btn.set_child(Some(&close_icon));
    hbox.append(&lbl);
    hbox.append(&close_btn);

    let notebook_clone = notebook.clone();
    let widget_clone = page_widget.upcast_ref::<Widget>().clone();
    close_btn.connect_clicked(move |_| {
        if let Some(page_num) = notebook_clone.page_num(&widget_clone) {
            notebook_clone.remove_page(Some(page_num));
        }
    });
    hbox
}
