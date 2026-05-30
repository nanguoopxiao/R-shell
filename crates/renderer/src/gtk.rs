/// GTK4 终端控件。
///
/// 使用 `DrawingArea` 和 Cairo CPU 优先渲染，保证在没有 GPU 加速的虚拟机中也能
/// 工作。Pango 用于字形测量，Cairo 用于实际绘制。
use std::cell::{Cell as BlinkCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4::pango::{
    FontDescription, Layout as PangoLayout, Style as PangoStyle, Weight as PangoWeight,
};
use gtk4::prelude::*;
use gtk4::{
    DrawingArea, EventControllerKey, EventControllerMotion, EventControllerScroll,
    EventControllerScrollFlags, GestureClick, GestureDrag,
};
use gtk4::{gdk, gio, glib};
use pangocairo::functions::show_layout;
use shell_core::TerminalSize;
use shell_terminal::{AnsiParser, AttrsFlags, Cell, Color, TerminalBuffer, TerminalSnapshot};

type InputHandler = Rc<RefCell<Option<Box<dyn Fn(Vec<u8>)>>>>;
type ResizeHandler = Rc<RefCell<Option<Box<dyn Fn(TerminalSize)>>>>;
type FontZoomHandler = Rc<RefCell<Option<Box<dyn Fn(i32)>>>>;
type SelectionState = Rc<RefCell<Option<Selection>>>;
type DragOrigin = Rc<RefCell<Option<(f64, f64)>>>;
type CursorBlinkState = Rc<BlinkCell<bool>>;
type HoveredUrlState = Rc<RefCell<Option<HoveredUrl>>>;

/// 鼠标滚轮每次滚动的行数。
const SCROLL_LINES: i64 = 3;
const CURSOR_BLINK_INTERVAL_MS: u64 = 530;
// 拉丁字母上伸/下伸部样本：避免每帧触发 CJK 字体回退加载。在中文 Windows 上，
// 频繁加载回退字体可能额外占用 70-100MB。
const METRIC_HEIGHT_SAMPLE: &str = "Mgj|";
const TERMINAL_BG: (f64, f64, f64) = (0.04, 0.045, 0.05);
const TERMINAL_FG: (f64, f64, f64) = (0.86, 0.88, 0.90);
const FONT_ZOOM_MIN_PT: f64 = 8.0;
const FONT_ZOOM_MAX_PT: f64 = 32.0;
const DEFAULT_FONT_SIZE_PT: f64 = 13.0;

#[derive(Clone, Debug)]
pub struct TerminalAppearance {
    // 同一视图的所有 `GtkTerminalView` 克隆共享该状态。更新设置只需修改这里并排队重绘，
    // 不需要重建 widget 树。
    font_description: Rc<RefCell<String>>,
    semantic_highlighting: Rc<RefCell<bool>>,
}

impl TerminalAppearance {
    #[must_use]
    pub fn new(font_description: impl Into<String>) -> Self {
        Self {
            font_description: Rc::new(RefCell::new(font_description.into())),
            semantic_highlighting: Rc::new(RefCell::new(true)),
        }
    }

    #[must_use]
    pub fn font_description(&self) -> String {
        self.font_description.borrow().clone()
    }

    pub fn set_font_description(&self, font_description: impl Into<String>) {
        *self.font_description.borrow_mut() = font_description.into();
    }

    #[must_use]
    pub fn semantic_highlighting(&self) -> bool {
        *self.semantic_highlighting.borrow()
    }

    pub fn set_semantic_highlighting(&self, enabled: bool) {
        *self.semantic_highlighting.borrow_mut() = enabled;
    }
}

impl Default for TerminalAppearance {
    fn default() -> Self {
        // 默认只指定一个等宽字体，避免启动时 Pango 探测系统字体列表中的全部 CJK
        // 回退字体。CJK/emoji 仍会通过 fallback_font_description() 正确渲染。
        Self::new("Cascadia Mono 13")
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CellMetrics {
    cell_width: f64,
    line_height: f64,
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self {
            cell_width: 8.0,
            line_height: 18.0,
        }
    }
}

/// 缓存的渲染状态。仅在字体描述或 widget 像素尺寸变化时重新计算，而不是每帧计算。
struct RenderCache {
    metrics: CellMetrics,
    font_description: FontDescription,
    /// 所有单元格、所有帧复用的单个 PangoLayout。复用布局并重置文本/字体，比为每个
    /// 单元格分配新的 GLib boxed 对象便宜得多。
    layout: PangoLayout,
    font_str: String,
    widget_px: (i32, i32),
}

type RenderCacheRef = Rc<RefCell<Option<RenderCache>>>;

#[derive(Clone)]
struct DrawState {
    render_cache: RenderCacheRef,
    cursor_blink_state: CursorBlinkState,
    hovered_url: HoveredUrlState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SemanticHighlight {
    fg: Color,
    bold: bool,
    priority: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HoveredUrl {
    line: usize,
    start_col: usize,
    end_col: usize,
    url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UrlRange {
    start: usize,
    end: usize,
    url: String,
}

#[derive(Debug)]
struct RenderedChar {
    byte_start: usize,
    byte_end: usize,
    col: usize,
    width: usize,
}

#[derive(Debug)]
struct RenderedLineText {
    text: String,
    chars: Vec<RenderedChar>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridPoint {
    line: usize,
    col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: GridPoint,
    focus: GridPoint,
}

impl Selection {
    fn normalized(self) -> (GridPoint, GridPoint) {
        if (self.anchor.line, self.anchor.col) <= (self.focus.line, self.focus.col) {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ViewLayout {
    // 终端历史中的可见切片。渲染和命中测试使用这些索引，避免在热路径克隆或扫描完整
    // 回滚历史。
    start_line: usize,
    total_lines: usize,
    rows: usize,
    cols: usize,
}

#[derive(Clone)]
pub struct GtkTerminalView {
    area: DrawingArea,
    buffer: Rc<RefCell<TerminalBuffer>>,
    parser: Rc<RefCell<AnsiParser>>,
    input_handler: InputHandler,
    resize_handler: ResizeHandler,
    font_zoom_handler: FontZoomHandler,
    appearance: TerminalAppearance,
    /// 当前视图相对底部向上滚动了多少条回滚历史行。
    scroll_offset: Rc<RefCell<i64>>,
    /// 缓存的单元格尺寸；字体或 widget 尺寸变化时失效。
    render_cache: RenderCacheRef,
    cursor_blink_state: CursorBlinkState,
}

impl GtkTerminalView {
    #[must_use]
    pub fn new(buffer: TerminalBuffer) -> Self {
        Self::with_appearance(buffer, TerminalAppearance::default())
    }

    #[must_use]
    pub fn with_appearance(buffer: TerminalBuffer, appearance: TerminalAppearance) -> Self {
        let area = DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        let buffer = Rc::new(RefCell::new(buffer));
        let parser = Rc::new(RefCell::new(AnsiParser::new()));
        let input_handler: InputHandler = Rc::new(RefCell::new(None));
        let resize_handler: ResizeHandler = Rc::new(RefCell::new(None));
        let font_zoom_handler: FontZoomHandler = Rc::new(RefCell::new(None));
        let scroll_offset: Rc<RefCell<i64>> = Rc::new(RefCell::new(0));
        let selection: SelectionState = Rc::new(RefCell::new(None));
        let render_cache: RenderCacheRef = Rc::new(RefCell::new(None));
        let cursor_blink_state: CursorBlinkState = Rc::new(BlinkCell::new(true));
        let hovered_url: HoveredUrlState = Rc::new(RefCell::new(None));
        let draw_state = DrawState {
            render_cache: Rc::clone(&render_cache),
            cursor_blink_state: Rc::clone(&cursor_blink_state),
            hovered_url: Rc::clone(&hovered_url),
        };

        connect_draw(
            &area,
            Rc::clone(&buffer),
            Rc::clone(&resize_handler),
            Rc::clone(&scroll_offset),
            Rc::clone(&selection),
            appearance.clone(),
            draw_state,
        );
        connect_keyboard(
            &area,
            Rc::clone(&input_handler),
            Rc::clone(&buffer),
            Rc::clone(&scroll_offset),
            Rc::clone(&selection),
            appearance.clone(),
            Rc::clone(&cursor_blink_state),
        );
        connect_scroll(
            &area,
            Rc::clone(&scroll_offset),
            Rc::clone(&buffer),
            Rc::clone(&font_zoom_handler),
        );
        connect_pointer(
            &area,
            Rc::clone(&buffer),
            Rc::clone(&scroll_offset),
            Rc::clone(&selection),
            Rc::clone(&render_cache),
            Rc::clone(&hovered_url),
        );
        connect_cursor_blink(&area, Rc::clone(&cursor_blink_state));

        Self {
            area,
            buffer,
            parser,
            input_handler,
            resize_handler,
            font_zoom_handler,
            appearance,
            scroll_offset,
            render_cache,
            cursor_blink_state,
        }
    }

    #[must_use]
    pub fn widget(&self) -> DrawingArea {
        self.area.clone()
    }

    pub fn set_input_handler(&self, handler: impl Fn(Vec<u8>) + 'static) {
        *self.input_handler.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_resize_handler(&self, handler: impl Fn(TerminalSize) + 'static) {
        *self.resize_handler.borrow_mut() = Some(Box::new(handler));
    }

    pub fn set_font_zoom_handler(&self, handler: impl Fn(i32) + 'static) {
        *self.font_zoom_handler.borrow_mut() = Some(Box::new(handler));
    }

    pub fn clear_io_handlers(&self) {
        *self.input_handler.borrow_mut() = None;
        *self.resize_handler.borrow_mut() = None;
        *self.font_zoom_handler.borrow_mut() = None;
    }

    pub fn set_font_description(&self, font_description: impl Into<String>) {
        self.appearance.set_font_description(font_description);
        // 字体变化后让尺寸缓存失效，下一帧重新测量。
        *self.render_cache.borrow_mut() = None;
        self.area.queue_draw();
    }

    pub fn feed(&self, bytes: &[u8]) {
        // 渲染器在 UI 边界负责解析：协议适配器只交付字节。收到新输出时总是回到实时尾部，
        // 这符合用户查看历史后终端继续输出时的常见行为。
        let mut buffer = self.buffer.borrow_mut();
        self.parser.borrow_mut().feed_bytes(&mut buffer, bytes);
        // 收到新数据后回到实时尾部视图。
        *self.scroll_offset.borrow_mut() = 0;
        self.cursor_blink_state.set(true);
        self.area.queue_draw();
    }
}

// ── Drawing ───────────────────────────────────────────────────────────────────

fn connect_draw(
    area: &DrawingArea,
    buffer: Rc<RefCell<TerminalBuffer>>,
    resize_handler: ResizeHandler,
    scroll_offset: Rc<RefCell<i64>>,
    selection: SelectionState,
    appearance: TerminalAppearance,
    draw_state: DrawState,
) {
    area.set_draw_func(move |widget, cr, width, height| {
        // 填充背景。
        cr.set_source_rgb(TERMINAL_BG.0, TERMINAL_BG.1, TERMINAL_BG.2);
        let _ = cr.paint();

        // 字体尺寸测量开销较高（会触发 Pango 字体加载），因此缓存结果；只有字体字符串
        // 或 widget 像素尺寸变化时才重新测量。
        let (metrics, font_description) =
            cached_cell_metrics(widget, &appearance, &draw_state.render_cache, width, height);
        let rows = visible_cell_count(height, metrics.line_height);
        let cols = visible_cell_count(width, metrics.cell_width);
        let terminal_size = TerminalSize::new(cols as u16, rows as u16);

        {
            let mut buf = buffer.borrow_mut();
            if buf.size() != terminal_size {
                buf.resize(terminal_size);
                if let Some(handler) = resize_handler.borrow().as_ref() {
                    handler(terminal_size);
                }
            }
        }

        let buffer = buffer.borrow();
        let semantic_highlighting_enabled = appearance.semantic_highlighting();
        let scrollback_len = buffer.scrollback_len();
        let layout = view_layout_from_counts(
            width,
            height,
            scrollback_len,
            buffer.total_line_count(),
            *scroll_offset.borrow(),
            metrics,
        );
        let selection = selection.borrow().as_ref().copied();
        let cursor = buffer.cursor_state();
        let should_paint_cursor = cursor_should_paint(
            widget.has_focus(),
            cursor.visible,
            draw_state.cursor_blink_state.get(),
        );

        // 借用缓存的 PangoLayout：它只在字体/尺寸变化时创建，不会每帧创建。缓存尚未
        // 预热的首帧会临时创建一个布局，理论上只发生一帧。
        let cache_guard = draw_state.render_cache.borrow();
        let tmp_layout;
        let shared_layout: &PangoLayout = if let Some(c) = cache_guard.as_ref() {
            &c.layout
        } else {
            tmp_layout = widget.create_pango_layout(None);
            &tmp_layout
        };

        for display_row in 0..rows {
            let line_idx = layout.start_line + display_row;
            let top = display_row as f64 * metrics.line_height;
            let is_screen_line = line_idx >= scrollback_len;

            if let Some(line) = buffer.line_at(line_idx) {
                let semantic_highlights = if semantic_highlighting_enabled {
                    semantic_highlights_for_line(line, cols)
                } else {
                    Vec::new()
                };
                for (col, cell) in line.iter().take(cols).enumerate() {
                    if cell.is_continuation() {
                        continue;
                    }
                    let x = col as f64 * metrics.cell_width;
                    let span = cell.display_width().max(1);
                    let cell_width = metrics.cell_width * span as f64;
                    let is_cursor = is_screen_line
                        && *scroll_offset.borrow() == 0
                        && should_paint_cursor
                        && (line_idx - scrollback_len) == cursor.pos.row
                        && (0..span).any(|offset| col + offset == cursor.pos.col);
                    let is_selected = selection
                        .map(|selected| {
                            (0..span)
                                .any(|offset| selection_contains(selected, line_idx, col + offset))
                        })
                        .unwrap_or(false);
                    let is_hovered_url =
                        draw_state.hovered_url.borrow().as_ref().is_some_and(|url| {
                            url.line == line_idx
                                && (col..col + span).any(|cell_col| {
                                    cell_col >= url.start_col && cell_col < url.end_col
                                })
                        });

                    let (mut fg, bg) = resolve_colors(cell, is_cursor, is_selected);
                    let mut paint_cell = *cell;
                    if let Some(highlight) = semantic_highlights.get(col).and_then(|entry| *entry)
                        && should_apply_semantic_highlight(cell, is_cursor, is_selected)
                    {
                        fg = highlight.fg;
                        if highlight.bold {
                            paint_cell.attrs.flags.insert(AttrsFlags::BOLD);
                        }
                    }
                    if is_hovered_url && !is_cursor && !is_selected {
                        paint_cell.attrs.flags.insert(AttrsFlags::UNDERLINE);
                    }
                    draw_cell_bg(cr, x, top, cell_width, metrics.line_height, bg);
                    draw_cell_text(
                        cr,
                        &paint_cell,
                        (x, top),
                        metrics,
                        &font_description,
                        fg,
                        shared_layout,
                    );
                }
            }
        }
    });
}

fn cursor_should_paint(has_focus: bool, cursor_visible: bool, blink_visible: bool) -> bool {
    cursor_visible && (!has_focus || blink_visible)
}

/// 为指定字体构建尺寸信息和新的 PangoLayout。
fn measure_cell_metrics(
    area: &DrawingArea,
    appearance: &TerminalAppearance,
) -> (CellMetrics, FontDescription, PangoLayout) {
    let font_description = base_font_description(appearance);
    let width_layout = area.create_pango_layout(Some("M"));
    width_layout.set_font_description(Some(&font_description));
    let (text_width, text_height) = width_layout.pixel_size();

    // 拉丁上伸/下伸部能提供可靠行高，并避免触发 CJK 字体加载（见 METRIC_HEIGHT_SAMPLE）。
    let height_layout = area.create_pango_layout(Some(METRIC_HEIGHT_SAMPLE));
    height_layout.set_font_description(Some(&font_description));
    let (_, fallback_height) = height_layout.pixel_size();

    // 共享布局会在每帧的每个单元格中复用。
    let shared = area.create_pango_layout(None);
    shared.set_font_description(Some(&font_description));

    (
        CellMetrics {
            cell_width: f64::from(text_width.max(1)),
            line_height: f64::from(text_height.max(fallback_height).max(1) + 4),
        },
        font_description,
        shared,
    )
}

/// 返回当前字体/widget 尺寸对应的缓存尺寸信息；只有二者之一变化时才重新测量，避免
/// 每帧分配 Pango layout。
fn cached_cell_metrics(
    area: &DrawingArea,
    appearance: &TerminalAppearance,
    cache: &RenderCacheRef,
    width: i32,
    height: i32,
) -> (CellMetrics, FontDescription) {
    let mut guard = cache.borrow_mut();
    let font_str = appearance.font_description();
    let valid = guard
        .as_ref()
        .is_some_and(|c| c.font_str == font_str && c.widget_px == (width, height));
    if valid {
        let c = guard.as_ref().unwrap();
        return (c.metrics, c.font_description.clone());
    }
    let (metrics, font_description, layout) = measure_cell_metrics(area, appearance);
    *guard = Some(RenderCache {
        metrics,
        font_description: font_description.clone(),
        layout,
        font_str,
        widget_px: (width, height),
    });
    (metrics, font_description)
}

fn base_font_description(appearance: &TerminalAppearance) -> FontDescription {
    FontDescription::from_string(&normalize_font_description(&appearance.font_description()))
}

fn normalize_font_description(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Cascadia Mono 13".to_string();
    }

    if !trimmed.contains(',') {
        return trimmed.to_string();
    }

    let mut digits = String::new();
    for ch in trimmed.chars().rev() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else if !digits.is_empty() {
            break;
        }
    }

    let size = if digits.is_empty() {
        "13".to_string()
    } else {
        digits.chars().rev().collect::<String>()
    };

    let family = trimmed
        .split(',')
        .next()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .unwrap_or("Cascadia Mono");

    format!("{family} {size}")
}

#[must_use]
pub fn adjusted_font_description_size(raw: &str, steps: i32) -> String {
    let trimmed = raw.trim();
    let source = if trimmed.is_empty() {
        "Cascadia Mono"
    } else {
        trimmed
    };

    let Some((prefix, size)) = split_font_description_size(source) else {
        let size =
            (DEFAULT_FONT_SIZE_PT + f64::from(steps)).clamp(FONT_ZOOM_MIN_PT, FONT_ZOOM_MAX_PT);
        return format!("{} {}", source.trim_end(), format_font_size(size));
    };

    let adjusted = (size + f64::from(steps)).clamp(FONT_ZOOM_MIN_PT, FONT_ZOOM_MAX_PT);
    format!("{} {}", prefix.trim_end(), format_font_size(adjusted))
}

fn split_font_description_size(raw: &str) -> Option<(&str, f64)> {
    let trimmed = raw.trim_end();
    let end = trimmed.len();
    let mut start = end;
    let mut seen_digit = false;
    let mut seen_dot = false;

    for (index, ch) in trimmed.char_indices().rev() {
        if ch.is_ascii_digit() {
            start = index;
            seen_digit = true;
        } else if ch == '.' && seen_digit && !seen_dot {
            start = index;
            seen_dot = true;
        } else {
            break;
        }
    }

    if !seen_digit || start == 0 {
        return None;
    }

    let prefix = &trimmed[..start];
    let size = trimmed[start..].parse::<f64>().ok()?;
    if prefix.trim().is_empty() {
        return None;
    }
    Some((prefix, size))
}

fn format_font_size(size: f64) -> String {
    if (size.fract()).abs() < f64::EPSILON {
        format!("{}", size as i32)
    } else {
        format!("{size:.1}")
    }
}

fn fallback_font_description(ch: char, base_font: &FontDescription) -> Option<FontDescription> {
    let family = if is_emoji(ch) {
        Some("Segoe UI Emoji")
    } else if is_rare_cjk(ch) {
        Some("SimSun-ExtB")
    } else if is_cjk(ch) {
        Some("Microsoft YaHei UI")
    } else if is_symbol_fallback(ch) {
        Some("Segoe UI Symbol")
    } else {
        None
    }?;

    let mut font = base_font.clone();
    font.set_family(family);
    Some(font)
}

fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2E80..=0x2EFF
            | 0x2F00..=0x2FDF
            | 0x3000..=0x303F
            | 0x3040..=0x30FF
            | 0x3100..=0x312F
            | 0x31A0..=0x31BF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xFE10..=0xFE1F
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFFEF
            | 0xF900..=0xFAFF
    )
}

fn is_rare_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x20000..=0x323AF)
}

fn is_emoji(ch: char) -> bool {
    matches!(ch as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE00..=0xFE0F)
}

fn is_symbol_fallback(ch: char) -> bool {
    matches!(ch as u32, 0x2190..=0x2BFF)
}

fn visible_cell_count(extent: i32, cell_extent: f64) -> usize {
    ((f64::from(extent.max(1)) / cell_extent.max(1.0)).floor() as usize).max(1)
}

fn should_apply_semantic_highlight(cell: &Cell, is_cursor: bool, is_selected: bool) -> bool {
    !is_cursor
        && !is_selected
        && cell.attrs.fg == Color::Default
        && !cell.attrs.flags.contains(AttrsFlags::INVERSE)
}

fn semantic_highlights_for_line(line: &[Cell], cols: usize) -> Vec<Option<SemanticHighlight>> {
    let visible_cols = cols.min(line.len());
    let rendered = rendered_line_text(line, visible_cols);
    let mut highlights = vec![None; visible_cols];

    if rendered.text.trim().is_empty() {
        return highlights;
    }

    highlight_labels(&rendered, &mut highlights);
    highlight_table_headers(&rendered, &mut highlights);
    highlight_environment_keys(&rendered, &mut highlights);
    highlight_assignment_keys(&rendered, &mut highlights);
    highlight_paths(&rendered, &mut highlights);
    highlight_numbers(&rendered, &mut highlights);
    highlight_split_size_units(&rendered, &mut highlights);
    highlight_http_status_codes(&rendered, &mut highlights);
    highlight_dates(&rendered, &mut highlights);
    highlight_keywords(&rendered, &mut highlights);
    highlight_log_levels(&rendered, &mut highlights);
    highlight_process_states(&rendered, &mut highlights);
    highlight_command_options(&rendered, &mut highlights);
    highlight_prompt_commands(&rendered, &mut highlights);
    highlight_process_names(&rendered, &mut highlights);
    highlight_addresses(&rendered, &mut highlights);
    highlight_urls(&rendered, &mut highlights);

    highlights
}

fn rendered_line_text(line: &[Cell], cols: usize) -> RenderedLineText {
    let mut text = String::with_capacity(cols);
    let mut chars = Vec::new();

    for (col, cell) in line.iter().take(cols).enumerate() {
        if cell.is_continuation() {
            continue;
        }

        let byte_start = text.len();
        text.push(cell.ch);
        let byte_end = text.len();
        chars.push(RenderedChar {
            byte_start,
            byte_end,
            col,
            width: cell.display_width().max(1),
        });
    }

    RenderedLineText { text, chars }
}

fn apply_highlight_range(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
    start: usize,
    end: usize,
    highlight: SemanticHighlight,
) {
    if start >= end {
        return;
    }

    for rendered_char in &rendered.chars {
        if rendered_char.byte_start >= end || rendered_char.byte_end <= start {
            continue;
        }

        let end_col = (rendered_char.col + rendered_char.width).min(highlights.len());
        for slot in highlights.iter_mut().take(end_col).skip(rendered_char.col) {
            if slot.is_none_or(|existing| existing.priority <= highlight.priority) {
                *slot = Some(highlight);
            }
        }
    }
}

fn highlight_urls(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for url in url_ranges(rendered.text.as_str()) {
        apply_highlight_range(rendered, highlights, url.start, url.end, url_highlight());
    }
}

fn url_ranges(text: &str) -> Vec<UrlRange> {
    let mut ranges = Vec::new();
    let mut search_start = 0;
    while let Some((start, prefix)) = find_next_url_prefix(text, search_start) {
        let mut end = start + prefix.len();
        for (offset, ch) in text[end..].char_indices() {
            if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | '[' | ']') {
                break;
            }
            end = start + prefix.len() + offset + ch.len_utf8();
        }
        while end > start && text[..end].ends_with(['.', ',', ';', ':', ')']) {
            end -= 1;
        }
        if end > start {
            let raw = &text[start..end];
            let url = if raw.starts_with("www.") {
                format!("https://{raw}")
            } else {
                raw.to_string()
            };
            ranges.push(UrlRange { start, end, url });
        }
        search_start = end.max(start + prefix.len());
    }
    ranges
}

fn find_next_url_prefix(text: &str, search_start: usize) -> Option<(usize, &'static str)> {
    ["https://", "http://", "www."]
        .into_iter()
        .filter_map(|prefix| {
            text[search_start..]
                .find(prefix)
                .map(|idx| (search_start + idx, prefix))
        })
        .min_by_key(|(idx, _)| *idx)
}

fn highlight_addresses(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if let Some((value_start, value_end, highlight)) = address_token_highlight(token) {
            apply_highlight_range(
                rendered,
                highlights,
                start + value_start,
                start + value_end,
                highlight,
            );
        }
    }
}

fn highlight_labels(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let token = &rendered.text[start..end];
        let Some(colon_idx) = token.rfind(':') else {
            continue;
        };
        if colon_idx == 0 || colon_idx + 1 != token.len() || token.len() > 40 {
            continue;
        }
        if token[..colon_idx]
            .chars()
            .any(|ch| ch.is_ascii_alphabetic())
        {
            apply_highlight_range(
                rendered,
                highlights,
                start,
                start + colon_idx + 1,
                label_highlight(),
            );
        }
    }
}

fn highlight_table_headers(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    if !line_looks_like_table_header(&rendered.text) {
        return;
    }

    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if is_table_header_word(token) {
            apply_highlight_range(rendered, highlights, start, end, table_header_highlight());
        }
    }
}

fn highlight_environment_keys(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        let Some(eq_idx) = token.find('=') else {
            continue;
        };
        let key = &token[..eq_idx];
        if is_environment_key(key) {
            apply_highlight_range(
                rendered,
                highlights,
                start,
                start + eq_idx,
                env_key_highlight(),
            );
        }
    }
}

fn highlight_assignment_keys(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        let Some(eq_idx) = token.find('=') else {
            continue;
        };
        let key = &token[..eq_idx];
        if is_assignment_key(key) {
            apply_highlight_range(
                rendered,
                highlights,
                start,
                start + eq_idx,
                label_highlight(),
            );
        }
    }
}

fn highlight_paths(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if let Some((value_start, value_end)) = path_token_range(token) {
            apply_highlight_range(
                rendered,
                highlights,
                start + value_start,
                start + value_end,
                path_highlight(),
            );
        }
    }
}

fn highlight_dates(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if is_month_or_weekday(token) || token.eq_ignore_ascii_case("UTC") {
            apply_highlight_range(rendered, highlights, start, end, date_highlight());
        }
    }
}

fn highlight_keywords(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if is_error_word(token) {
            apply_highlight_range(rendered, highlights, start, end, error_highlight());
        } else if is_warning_word(token) {
            apply_highlight_range(rendered, highlights, start, end, warning_highlight());
        } else if is_success_word(token) {
            apply_highlight_range(rendered, highlights, start, end, success_highlight());
        }
    }
}

fn highlight_log_levels(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        let highlight = if is_trace_or_debug_level(token) {
            Some(debug_highlight())
        } else if is_info_level(token) {
            Some(info_highlight())
        } else if is_warning_level(token) {
            Some(warning_highlight())
        } else if is_error_level(token) {
            Some(error_highlight())
        } else {
            None
        };
        if let Some(highlight) = highlight {
            apply_highlight_range(rendered, highlights, start, end, highlight);
        }
    }
}

fn highlight_process_states(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    if !line_looks_like_process_table(&rendered.text) {
        return;
    }

    for (start, end) in token_ranges(&rendered.text) {
        let token = &rendered.text[start..end];
        if is_process_state(token) {
            apply_highlight_range(rendered, highlights, start, end, process_state_highlight());
        }
    }
}

fn highlight_command_options(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if is_command_option(token) {
            apply_highlight_range(rendered, highlights, start, end, command_option_highlight());
        }
    }
}

fn highlight_prompt_commands(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    let text = rendered.text.as_str();
    for marker in ["# ", "$ ", "> ", "run:", "Run:"] {
        if let Some(marker_start) = text.rfind(marker) {
            let mut start = marker_start + marker.len();
            while let Some(ch) = text[start..].chars().next()
                && ch.is_whitespace()
            {
                start += ch.len_utf8();
            }
            highlight_command_at(rendered, highlights, start);
            return;
        }
    }

    if let Some((start, _)) = token_ranges(text).first().copied() {
        highlight_command_at(rendered, highlights, start);
    }
}

fn highlight_command_at(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
    start: usize,
) {
    if start >= rendered.text.len() {
        return;
    }

    let mut end = start;
    for (offset, ch) in rendered.text[start..].char_indices() {
        if is_command_char(ch) {
            end = start + offset + ch.len_utf8();
        } else {
            break;
        }
    }

    if end > start && is_common_command(&rendered.text[start..end]) {
        apply_highlight_range(rendered, highlights, start, end, command_highlight());
    }
}

fn highlight_numbers(rendered: &RenderedLineText, highlights: &mut [Option<SemanticHighlight>]) {
    let text = rendered.text.as_str();
    let mut index = 0;
    while index < text.len() {
        let Some(ch) = text[index..].chars().next() else {
            break;
        };

        if !ch.is_ascii_digit() || is_embedded_in_word(text, index) {
            index += ch.len_utf8();
            continue;
        }

        let Some((end, number_kind)) = parse_number_like(text, index) else {
            index += ch.len_utf8();
            continue;
        };

        let highlight = match number_kind {
            NumberKind::Size => size_highlight(),
            NumberKind::Percent | NumberKind::Number => number_highlight(),
        };
        apply_highlight_range(rendered, highlights, index, end, highlight);
        index = end;
    }
}

fn highlight_split_size_units(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    let ranges = token_ranges(&rendered.text);
    for pair in ranges.windows(2) {
        let (number_start, number_end) =
            trim_token_punctuation(&rendered.text, pair[0].0, pair[0].1);
        let (unit_start, unit_end) = trim_token_punctuation(&rendered.text, pair[1].0, pair[1].1);
        if number_start >= number_end || unit_start >= unit_end {
            continue;
        }
        let number = &rendered.text[number_start..number_end];
        let unit = &rendered.text[unit_start..unit_end];
        if is_plain_decimal_number(number) && is_size_unit_token(unit) {
            apply_highlight_range(
                rendered,
                highlights,
                number_start,
                number_end,
                size_highlight(),
            );
            apply_highlight_range(rendered, highlights, unit_start, unit_end, size_highlight());
        }
    }
}

fn highlight_http_status_codes(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    if !has_http_status_context(&rendered.text) {
        return;
    }

    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if let Some((value_start, value_end, highlight)) = http_status_token_highlight(token) {
            apply_highlight_range(
                rendered,
                highlights,
                start + value_start,
                start + value_end,
                highlight,
            );
        }
    }
}

fn highlight_process_names(
    rendered: &RenderedLineText,
    highlights: &mut [Option<SemanticHighlight>],
) {
    for (start, end) in token_ranges(&rendered.text) {
        let (start, end) = trim_token_punctuation(&rendered.text, start, end);
        if start >= end {
            continue;
        }
        let token = &rendered.text[start..end];
        if is_common_process_name(token) {
            apply_highlight_range(rendered, highlights, start, end, process_highlight());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberKind {
    Number,
    Percent,
    Size,
}

fn parse_number_like(text: &str, start: usize) -> Option<(usize, NumberKind)> {
    let mut end = start;
    let mut saw_digit = false;
    let bytes = text.as_bytes();

    while end < text.len() {
        let ch = text[end..].chars().next()?;
        if ch.is_ascii_digit() {
            saw_digit = true;
            end += ch.len_utf8();
            continue;
        }
        if matches!(ch, '.' | ':' | '-')
            && end + 1 < text.len()
            && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
        {
            end += ch.len_utf8();
            continue;
        }
        break;
    }

    if !saw_digit || end == start {
        return None;
    }

    if text[end..].starts_with('%') {
        return Some((end + 1, NumberKind::Percent));
    }

    if let Some(unit_end) = parse_size_unit(text, end) {
        return Some((unit_end, NumberKind::Size));
    }

    Some((end, NumberKind::Number))
}

fn parse_size_unit(text: &str, start: usize) -> Option<usize> {
    let rest = &text[start..];
    [
        "KiB", "MiB", "GiB", "TiB", "PiB", "KB", "MB", "GB", "TB", "PB", "K", "M", "G", "T", "P",
    ]
    .into_iter()
    .find_map(|unit| rest.starts_with(unit).then_some(start + unit.len()))
}

fn token_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut token_start = None;

    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = token_start.take() {
                ranges.push((start, idx));
            }
        } else if token_start.is_none() {
            token_start = Some(idx);
        }
    }

    if let Some(start) = token_start {
        ranges.push((start, text.len()));
    }

    ranges
}

fn trim_token_punctuation(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end {
        let Some(ch) = text[start..end].chars().next() else {
            break;
        };
        if matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'') {
            start += ch.len_utf8();
        } else {
            break;
        }
    }

    while start < end {
        let Some(ch) = text[start..end].chars().next_back() else {
            break;
        };
        if matches!(ch, ')' | ']' | '}' | '>' | ',' | ';' | '"' | '\'') {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }

    (start, end)
}

fn address_token_highlight(token: &str) -> Option<(usize, usize, SemanticHighlight)> {
    if is_ipv4_address(token) {
        return Some((0, token.len(), ip_highlight()));
    }
    if is_ipv6_address_like(token) {
        return Some((0, token.len(), ipv6_highlight()));
    }

    if let Some(separator) = token.find('=') {
        let value_start = separator + 1;
        let value = &token[value_start..];
        if is_ipv4_address(value) {
            return Some((value_start, token.len(), ip_highlight()));
        }
        if is_ipv6_address_like(value) {
            return Some((value_start, token.len(), ipv6_highlight()));
        }
    }

    if let Some(separator) = token.find(':') {
        let key = &token[..separator];
        if is_assignment_key(key) {
            let value_start = separator + 1;
            let value = &token[value_start..];
            if is_ipv4_address(value) {
                return Some((value_start, token.len(), ip_highlight()));
            }
            if is_ipv6_address_like(value) {
                return Some((value_start, token.len(), ipv6_highlight()));
            }
        }
    }

    None
}

fn path_token_range(token: &str) -> Option<(usize, usize)> {
    if is_path_like(token) {
        return Some((0, token.len()));
    }

    let separator = token.find('=')?;
    let value_start = separator + 1;
    let value = &token[value_start..];
    is_path_like(value).then_some((value_start, token.len()))
}

fn is_path_like(value: &str) -> bool {
    value.len() > 1
        && ((value.starts_with('/') && !value.starts_with("//"))
            || value.starts_with("~/")
            || value.starts_with("./")
            || value.starts_with("../")
            || is_windows_path(value))
}

fn is_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn is_plain_decimal_number(token: &str) -> bool {
    let mut saw_digit = false;
    let mut saw_dot = false;
    for ch in token.chars() {
        if ch.is_ascii_digit() {
            saw_digit = true;
        } else if ch == '.' && !saw_dot {
            saw_dot = true;
        } else {
            return false;
        }
    }
    saw_digit
}

fn is_size_unit_token(token: &str) -> bool {
    matches!(
        token,
        "B" | "K"
            | "M"
            | "G"
            | "T"
            | "P"
            | "KB"
            | "MB"
            | "GB"
            | "TB"
            | "PB"
            | "KiB"
            | "MiB"
            | "GiB"
            | "TiB"
            | "PiB"
            | "bytes"
            | "byte"
    )
}

fn is_assignment_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && key.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn line_looks_like_table_header(text: &str) -> bool {
    let mut header_words = 0usize;
    for (start, end) in token_ranges(text) {
        let (start, end) = trim_token_punctuation(text, start, end);
        if start < end && is_table_header_word(&text[start..end]) {
            header_words += 1;
        }
    }
    header_words >= 3
}

fn line_looks_like_process_table(text: &str) -> bool {
    if text.contains("%CPU") || text.contains("%MEM") || text.contains("COMMAND") {
        return true;
    }

    let tokens: Vec<&str> = token_ranges(text)
        .into_iter()
        .filter_map(|(start, end)| {
            let (start, end) = trim_token_punctuation(text, start, end);
            (start < end).then_some(&text[start..end])
        })
        .collect();

    tokens.len() >= 8
        && tokens
            .first()
            .is_some_and(|token| token.chars().all(|ch| ch.is_ascii_digit()))
        && tokens.iter().any(|token| is_common_process_name(token))
}

fn is_embedded_in_word(text: &str, index: usize) -> bool {
    if index == 0 {
        return false;
    }
    text[..index]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}

fn is_ipv4_address(token: &str) -> bool {
    let mut parts = token.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let mut count = 1;
    if !is_ipv4_octet(first) {
        return false;
    }
    for part in parts {
        count += 1;
        if !is_ipv4_octet(part) {
            return false;
        }
    }
    count == 4
}

fn is_ipv4_octet(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 3
        && part.chars().all(|ch| ch.is_ascii_digit())
        && part.parse::<u8>().is_ok()
}

fn is_ipv6_address_like(token: &str) -> bool {
    let colon_count = token.chars().filter(|ch| *ch == ':').count();
    if colon_count < 2 {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_hexdigit() || ch == ':') {
        return false;
    }
    token.contains("::")
        || colon_count >= 3
        || token.chars().any(|ch| matches!(ch, 'a'..='f' | 'A'..='F'))
}

fn is_month_or_weekday(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "mon"
            | "monday"
            | "tue"
            | "tues"
            | "tuesday"
            | "wed"
            | "wednesday"
            | "thu"
            | "thur"
            | "thurs"
            | "thursday"
            | "fri"
            | "friday"
            | "sat"
            | "saturday"
            | "sun"
            | "sunday"
            | "jan"
            | "january"
            | "feb"
            | "february"
            | "mar"
            | "march"
            | "apr"
            | "april"
            | "may"
            | "jun"
            | "june"
            | "jul"
            | "july"
            | "aug"
            | "august"
            | "sep"
            | "sept"
            | "september"
            | "oct"
            | "october"
            | "nov"
            | "november"
            | "dec"
            | "december"
    )
}

fn is_error_word(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "error" | "failed" | "failure" | "fatal" | "panic" | "denied" | "refused" | "unreachable"
    )
}

fn is_warning_word(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "warn" | "warning" | "not" | "required" | "restart" | "security" | "updates"
    )
}

fn is_trace_or_debug_level(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "trace" | "trc" | "debug" | "dbg"
    )
}

fn is_info_level(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "info" | "information" | "notice"
    )
}

fn is_warning_level(token: &str) -> bool {
    matches!(token.to_ascii_lowercase().as_str(), "warn" | "warning")
}

fn is_error_level(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "error" | "err" | "fatal" | "crit" | "critical" | "panic"
    )
}

fn is_success_word(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "ok" | "ready" | "active" | "enabled" | "connected" | "running" | "free"
    )
}

fn is_command_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')
}

fn is_command_option(token: &str) -> bool {
    token.len() > 1
        && token.starts_with('-')
        && token.chars().skip(1).any(|ch| ch.is_ascii_alphanumeric())
        && token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '=' | '.' | ':' | '/'))
}

fn is_process_state(token: &str) -> bool {
    matches!(token, "R" | "S" | "D" | "I" | "T" | "t" | "Z" | "X" | "W")
}

fn is_environment_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 32
        && key
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch == '_' || ch.is_ascii_digit())
        && key.chars().any(|ch| ch.is_ascii_uppercase())
}

fn is_table_header_word(token: &str) -> bool {
    matches!(
        token,
        "PID"
            | "USER"
            | "UID"
            | "PPID"
            | "PRI"
            | "PR"
            | "NI"
            | "VIRT"
            | "RES"
            | "SHR"
            | "S"
            | "%CPU"
            | "%MEM"
            | "TIME"
            | "TIME+"
            | "COMMAND"
            | "CMD"
            | "Filesystem"
            | "Type"
            | "Size"
            | "Used"
            | "Avail"
            | "Use%"
            | "Mounted"
            | "on"
            | "NAME"
            | "MAJ:MIN"
            | "RM"
            | "RO"
            | "MOUNTPOINT"
            | "Proto"
            | "Recv-Q"
            | "Send-Q"
            | "Local"
            | "Address"
            | "Foreign"
            | "State"
    )
}

fn has_http_status_context(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("http")
        || lower.contains("status")
        || lower.contains("response")
        || lower.contains("request")
}

fn http_status_highlight(token: &str) -> Option<SemanticHighlight> {
    if token.len() != 3 || !token.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    match token.parse::<u16>().ok()? {
        100..=199 => Some(info_highlight()),
        200..=299 => Some(success_highlight()),
        300..=399 => Some(info_highlight()),
        400..=499 => Some(warning_highlight()),
        500..=599 => Some(error_highlight()),
        _ => None,
    }
}

fn http_status_token_highlight(token: &str) -> Option<(usize, usize, SemanticHighlight)> {
    if let Some(highlight) = http_status_highlight(token) {
        return Some((0, token.len(), highlight));
    }

    let separator = token.find('=').or_else(|| token.find(':'))?;
    let key = &token[..separator].to_ascii_lowercase();
    if !matches!(
        key.as_str(),
        "status" | "status_code" | "code" | "http_status"
    ) {
        return None;
    }
    let value_start = separator + 1;
    let value = &token[value_start..];
    let highlight = http_status_highlight(value)?;
    Some((value_start, token.len(), highlight))
}

fn is_common_command(command: &str) -> bool {
    let command = command.rsplit('/').next().unwrap_or(command);
    matches!(
        command,
        "apt"
            | "apt-get"
            | "cat"
            | "chmod"
            | "chown"
            | "cp"
            | "curl"
            | "df"
            | "dnf"
            | "docker"
            | "du"
            | "find"
            | "free"
            | "grep"
            | "htop"
            | "hostname"
            | "ifconfig"
            | "ip"
            | "journalctl"
            | "kill"
            | "kubectl"
            | "less"
            | "ls"
            | "mkdir"
            | "mount"
            | "mv"
            | "nano"
            | "netstat"
            | "ping"
            | "ps"
            | "pwd"
            | "reboot"
            | "rm"
            | "rmdir"
            | "route"
            | "scp"
            | "sftp"
            | "shutdown"
            | "ss"
            | "ssh"
            | "systemctl"
            | "tail"
            | "tar"
            | "top"
            | "touch"
            | "traceroute"
            | "uname"
            | "umount"
            | "vim"
            | "wget"
            | "whoami"
            | "yum"
    )
}

fn is_common_process_name(token: &str) -> bool {
    let name = token.rsplit('/').next().unwrap_or(token);
    let name = name.trim_end_matches(|ch: char| ch.is_ascii_digit());
    matches!(
        name,
        "bash"
            | "containerd"
            | "cron"
            | "dbus-daemon"
            | "dockerd"
            | "git"
            | "java"
            | "kubelet"
            | "nginx"
            | "node"
            | "postgres"
            | "python"
            | "redis-server"
            | "rsyslogd"
            | "shell-app"
            | "sshd"
            | "systemd"
            | "top"
            | "vim"
            | "zsh"
    )
}

fn label_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xC8, 0xCF, 0xD8),
        bold: true,
        priority: 12,
    }
}

fn table_header_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xF0, 0xF3, 0xF6),
        bold: true,
        priority: 28,
    }
}

fn env_key_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xBD, 0x93, 0xF9),
        bold: true,
        priority: 29,
    }
}

fn number_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x3D, 0xF5, 0xE9),
        bold: true,
        priority: 20,
    }
}

fn size_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xF2, 0xEE, 0x9A),
        bold: true,
        priority: 21,
    }
}

fn url_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xFF, 0xB8, 0x3D),
        bold: true,
        priority: 60,
    }
}

fn ip_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xE9, 0xB3, 0xFF),
        bold: true,
        priority: 50,
    }
}

fn ipv6_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x62, 0xE6, 0xD6),
        bold: true,
        priority: 50,
    }
}

fn path_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x9E, 0xC7, 0xFF),
        bold: false,
        priority: 10,
    }
}

fn date_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x8D, 0xFF, 0x2E),
        bold: true,
        priority: 30,
    }
}

fn command_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xFF, 0xD1, 0x66),
        bold: true,
        priority: 40,
    }
}

fn command_option_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x7A, 0xB3, 0xFF),
        bold: true,
        priority: 38,
    }
}

fn process_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xC7, 0xD0, 0xDD),
        bold: true,
        priority: 32,
    }
}

fn process_state_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x9E, 0xC7, 0xFF),
        bold: true,
        priority: 31,
    }
}

fn debug_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x9A, 0xA6, 0xB2),
        bold: true,
        priority: 45,
    }
}

fn info_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x7A, 0xB3, 0xFF),
        bold: true,
        priority: 45,
    }
}

fn error_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xFF, 0x6B, 0x6B),
        bold: true,
        priority: 55,
    }
}

fn warning_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0xFF, 0xCF, 0x5F),
        bold: true,
        priority: 35,
    }
}

fn success_highlight() -> SemanticHighlight {
    SemanticHighlight {
        fg: Color::Rgb(0x7E, 0xD3, 0x8B),
        bold: true,
        priority: 35,
    }
}

/// 在应用 INVERSE 和光标覆盖后解析最终前景/背景色。
fn resolve_colors(cell: &Cell, is_cursor: bool, is_selected: bool) -> (Color, Color) {
    let mut fg = cell.attrs.fg;
    let mut bg = cell.attrs.bg;

    if cell.attrs.flags.contains(AttrsFlags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
        if fg == Color::Default {
            fg = Color::Indexed(0);
        }
        if bg == Color::Default {
            bg = Color::Rgb(0xDA, 0xE0, 0xE6);
        }
    }

    if is_cursor {
        // 光标：反转前景/背景，并使用高亮色。
        let cursor_bg = Color::Rgb(0xA8, 0xC0, 0xFF);
        let cursor_fg = Color::Indexed(0);
        (cursor_fg, cursor_bg)
    } else if is_selected {
        (Color::Rgb(0xEE, 0xF1, 0xF5), Color::Rgb(0x2E, 0x5C, 0x85))
    } else {
        (fg, bg)
    }
}

fn draw_cell_bg(
    cr: &gtk4::cairo::Context,
    x: f64,
    top: f64,
    cell_width: f64,
    line_height: f64,
    bg: Color,
) {
    if bg == Color::Default {
        return; // 背景已由终端底色填充。
    }
    set_source_color(cr, bg, true);
    cr.rectangle(x, top, cell_width, line_height);
    let _ = cr.fill();
}

fn draw_cell_text(
    cr: &gtk4::cairo::Context,
    cell: &Cell,
    origin: (f64, f64),
    metrics: CellMetrics,
    base_font: &FontDescription,
    fg: Color,
    layout: &PangoLayout,
) {
    if cell.ch == ' ' || cell.is_continuation() {
        return;
    }

    let (x, top) = origin;
    let cell_width = metrics.cell_width * cell.display_width().max(1) as f64;

    let mut font = base_font.clone();
    font.set_weight(if cell.attrs.flags.contains(AttrsFlags::BOLD) {
        PangoWeight::Bold
    } else {
        PangoWeight::Normal
    });
    font.set_style(if cell.attrs.flags.contains(AttrsFlags::ITALIC) {
        PangoStyle::Italic
    } else {
        PangoStyle::Normal
    });
    if let Some(fallback_font) = fallback_font_description(cell.ch, &font) {
        font = fallback_font;
    }
    layout.set_font_description(Some(&font));
    layout.set_text(&cell.ch.to_string());

    let (_text_width, text_height) = layout.pixel_size();
    let text_y = top + ((metrics.line_height - f64::from(text_height)).max(0.0) / 2.0);
    set_source_color(cr, fg, false);
    cr.move_to(x, text_y);
    show_layout(cr, layout);
    // 下划线。
    if cell.attrs.flags.contains(AttrsFlags::UNDERLINE) {
        let underline_y = top + metrics.line_height - 2.0;
        cr.move_to(x, underline_y);
        cr.line_to(x + cell_width, underline_y);
        let _ = cr.stroke();
    }
}

fn set_source_color(cr: &gtk4::cairo::Context, color: Color, is_bg: bool) {
    match color {
        Color::Default => {
            if is_bg {
                cr.set_source_rgb(TERMINAL_BG.0, TERMINAL_BG.1, TERMINAL_BG.2);
            } else {
                cr.set_source_rgb(TERMINAL_FG.0, TERMINAL_FG.1, TERMINAL_FG.2);
            }
        }
        Color::Rgb(r, g, b) => cr.set_source_rgb(
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        ),
        Color::Indexed(i) => {
            let (r, g, b) = indexed_to_rgb(i);
            cr.set_source_rgb(
                f64::from(r) / 255.0,
                f64::from(g) / 255.0,
                f64::from(b) / 255.0,
            );
        }
    }
}

/// 将 xterm 256 色索引转换为 (r, g, b)。
fn indexed_to_rgb(idx: u8) -> (u8, u8, u8) {
    match idx {
        // 标准 8 色（暗色）。
        0 => (0x1a, 0x1b, 0x1e),
        1 => (0xec, 0x40, 0x40),
        2 => (0x59, 0xc0, 0x6a),
        3 => (0xdb, 0xa8, 0x40),
        4 => (0x5a, 0x8e, 0xe8),
        5 => (0xc2, 0x66, 0xdb),
        6 => (0x4d, 0xb4, 0xc6),
        7 => (0xda, 0xe0, 0xe6),
        // 高亮 8 色。
        8 => (0x3e, 0x41, 0x52),
        9 => (0xf2, 0x67, 0x67),
        10 => (0x7e, 0xd3, 0x8b),
        11 => (0xe8, 0xc9, 0x70),
        12 => (0x7e, 0xa8, 0xf5),
        13 => (0xd4, 0x8c, 0xf0),
        14 => (0x77, 0xcc, 0xdc),
        15 => (0xee, 0xf1, 0xf5),
        // 6×6×6 色彩立方（16-231）。
        16..=231 => {
            let n = idx - 16;
            let b = (n % 6) * 51;
            let g = ((n / 6) % 6) * 51;
            let r = (n / 36) * 51;
            (r, g, b)
        }
        // 灰阶 ramp（232-255）。
        _ => {
            let v = (idx - 232) * 10 + 8;
            (v, v, v)
        }
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────────────

fn connect_keyboard(
    area: &DrawingArea,
    input_handler: InputHandler,
    buffer: Rc<RefCell<TerminalBuffer>>,
    scroll_offset: Rc<RefCell<i64>>,
    selection: SelectionState,
    appearance: TerminalAppearance,
    cursor_blink_state: CursorBlinkState,
) {
    let controller = EventControllerKey::new();
    let area_for_keys = area.clone();
    controller.connect_key_pressed(move |_, key, _code, mods| {
        if mods.contains(gdk::ModifierType::CONTROL_MASK) {
            match key {
                gdk::Key::C | gdk::Key::c if has_copyable_selection(&selection) => {
                    copy_selection_to_clipboard(
                        &area_for_keys,
                        &buffer.borrow(),
                        *scroll_offset.borrow(),
                        &selection,
                        &appearance,
                    );
                    return glib::Propagation::Stop;
                }
                gdk::Key::V | gdk::Key::v => {
                    cursor_blink_state.set(true);
                    area_for_keys.queue_draw();
                    paste_clipboard_text(Rc::clone(&input_handler));
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }

        if let Some(bytes) = key_to_bytes(key, mods) {
            cursor_blink_state.set(true);
            area_for_keys.queue_draw();
            if let Some(handler) = input_handler.borrow().as_ref() {
                handler(bytes);
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    area.add_controller(controller);
}

fn connect_cursor_blink(area: &DrawingArea, cursor_blink_state: CursorBlinkState) {
    let area_for_focus = area.clone();
    let blink_for_focus = Rc::clone(&cursor_blink_state);
    area.connect_has_focus_notify(move |_| {
        blink_for_focus.set(true);
        area_for_focus.queue_draw();
    });

    let area_weak = {
        let weak = glib::WeakRef::<DrawingArea>::new();
        weak.set(Some(area));
        weak
    };
    glib::timeout_add_local(Duration::from_millis(CURSOR_BLINK_INTERVAL_MS), move || {
        let Some(area) = area_weak.upgrade() else {
            return glib::ControlFlow::Break;
        };

        if area.has_focus() {
            cursor_blink_state.set(!cursor_blink_state.get());
            area.queue_draw();
        } else if !cursor_blink_state.get() {
            cursor_blink_state.set(true);
            area.queue_draw();
        }

        glib::ControlFlow::Continue
    });
}

fn has_copyable_selection(selection: &SelectionState) -> bool {
    selection
        .borrow()
        .as_ref()
        .is_some_and(|selected| selected.anchor != selected.focus)
}

fn connect_pointer(
    area: &DrawingArea,
    buffer: Rc<RefCell<TerminalBuffer>>,
    scroll_offset: Rc<RefCell<i64>>,
    selection: SelectionState,
    render_cache: RenderCacheRef,
    hovered_url: HoveredUrlState,
) {
    let drag_origin: DragOrigin = Rc::new(RefCell::new(None));
    let render_cache_begin = Rc::clone(&render_cache);
    let render_cache_update = Rc::clone(&render_cache);

    let motion = EventControllerMotion::new();
    let area_for_motion = area.clone();
    let buffer_for_motion = Rc::clone(&buffer);
    let scroll_for_motion = Rc::clone(&scroll_offset);
    let cache_for_motion = Rc::clone(&render_cache);
    let hovered_for_motion = Rc::clone(&hovered_url);
    motion.connect_motion(move |_, x, y| {
        let next_url = url_at_point(
            &area_for_motion,
            &buffer_for_motion.borrow(),
            *scroll_for_motion.borrow(),
            x,
            y,
            &cache_for_motion,
        );
        let changed = *hovered_for_motion.borrow() != next_url;
        if changed {
            *hovered_for_motion.borrow_mut() = next_url;
            area_for_motion.queue_draw();
        }
        if hovered_for_motion.borrow().is_some() {
            area_for_motion.set_cursor_from_name(Some("pointer"));
        } else {
            area_for_motion.set_cursor_from_name(None);
        }
    });
    let area_for_leave = area.clone();
    let hovered_for_leave = Rc::clone(&hovered_url);
    motion.connect_leave(move |_| {
        if hovered_for_leave.borrow_mut().take().is_some() {
            area_for_leave.queue_draw();
        }
        area_for_leave.set_cursor_from_name(None);
    });
    area.add_controller(motion);

    let click = GestureClick::new();
    click.set_button(1);
    let area_for_click = area.clone();
    let buffer_for_click = Rc::clone(&buffer);
    let scroll_for_click = Rc::clone(&scroll_offset);
    let cache_for_click = Rc::clone(&render_cache);
    click.connect_released(move |_, _presses, x, y| {
        if let Some(url) = url_at_point(
            &area_for_click,
            &buffer_for_click.borrow(),
            *scroll_for_click.borrow(),
            x,
            y,
            &cache_for_click,
        ) && let Err(err) =
            gio::AppInfo::launch_default_for_uri(&url.url, None::<&gio::AppLaunchContext>)
        {
            eprintln!("failed to open terminal URL {}: {err}", url.url);
        }
    });
    area.add_controller(click);

    let drag = GestureDrag::new();
    drag.set_button(1);
    let area_for_begin = area.clone();
    let buffer_for_begin = Rc::clone(&buffer);
    let scroll_for_begin = Rc::clone(&scroll_offset);
    let selection_for_begin = Rc::clone(&selection);
    let drag_origin_for_begin = Rc::clone(&drag_origin);
    drag.connect_drag_begin(move |_, x, y| {
        area_for_begin.grab_focus();
        *drag_origin_for_begin.borrow_mut() = Some((x, y));
        if let Some(point) = grid_point_at(
            &area_for_begin,
            &buffer_for_begin.borrow(),
            *scroll_for_begin.borrow(),
            x,
            y,
            &render_cache_begin,
        ) {
            *selection_for_begin.borrow_mut() = Some(Selection {
                anchor: point,
                focus: point,
            });
            area_for_begin.queue_draw();
        }
    });

    let area_for_update = area.clone();
    let buffer_for_update = Rc::clone(&buffer);
    let scroll_for_update = Rc::clone(&scroll_offset);
    let selection_for_update = Rc::clone(&selection);
    let drag_origin_for_update = Rc::clone(&drag_origin);
    drag.connect_drag_update(move |_, offset_x, offset_y| {
        let Some((start_x, start_y)) = *drag_origin_for_update.borrow() else {
            return;
        };
        let x = start_x + offset_x;
        let y = start_y + offset_y;
        if let Some(point) = grid_point_at(
            &area_for_update,
            &buffer_for_update.borrow(),
            *scroll_for_update.borrow(),
            x,
            y,
            &render_cache_update,
        ) && let Some(selection) = selection_for_update.borrow_mut().as_mut()
        {
            selection.focus = point;
            area_for_update.queue_draw();
        }
    });

    let area_for_end = area.clone();
    let selection_for_end = Rc::clone(&selection);
    let drag_origin_for_end = Rc::clone(&drag_origin);
    drag.connect_drag_end(move |_, _offset_x, _offset_y| {
        *drag_origin_for_end.borrow_mut() = None;
        let should_clear = selection_for_end
            .borrow()
            .as_ref()
            .is_some_and(|selection| selection.anchor == selection.focus);
        if should_clear {
            selection_for_end.borrow_mut().take();
            area_for_end.queue_draw();
        }
    });
    area.add_controller(drag);
}

fn copy_selection_to_clipboard(
    area: &DrawingArea,
    buffer: &TerminalBuffer,
    scroll_offset: i64,
    selection: &SelectionState,
    appearance: &TerminalAppearance,
) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let snapshot = buffer.snapshot();
    let Some(selected) = selection.borrow().as_ref().copied() else {
        return;
    };
    let text = extract_selection_text(
        &snapshot,
        selected,
        view_layout(
            area.allocated_width(),
            area.allocated_height(),
            &snapshot,
            scroll_offset,
            measure_cell_metrics(area, appearance).0,
        ),
    );
    if !text.is_empty() {
        display.clipboard().set_text(&text);
    }
}

fn paste_clipboard_text(input_handler: InputHandler) {
    let Some(display) = gdk::Display::default() else {
        return;
    };

    display
        .clipboard()
        .read_text_async(None::<&gtk4::gio::Cancellable>, move |result| {
            if let Ok(Some(text)) = result
                && let Some(handler) = input_handler.borrow().as_ref()
            {
                handler(text.as_str().as_bytes().to_vec());
            }
        });
}

fn selection_contains(selection: Selection, line: usize, col: usize) -> bool {
    let (start, end) = selection.normalized();
    if line < start.line || line > end.line {
        return false;
    }
    if start.line == end.line {
        return (start.col..=end.col).contains(&col);
    }
    if line == start.line {
        return col >= start.col;
    }
    if line == end.line {
        return col <= end.col;
    }
    true
}

fn view_layout(
    width: i32,
    height: i32,
    snapshot: &TerminalSnapshot,
    scroll_offset: i64,
    metrics: CellMetrics,
) -> ViewLayout {
    view_layout_from_counts(
        width,
        height,
        snapshot.scrollback.len(),
        snapshot.scrollback.len() + snapshot.lines.len(),
        scroll_offset,
        metrics,
    )
}

fn view_layout_from_counts(
    width: i32,
    height: i32,
    scrollback_len: usize,
    total_lines: usize,
    scroll_offset: i64,
    metrics: CellMetrics,
) -> ViewLayout {
    let rows = visible_cell_count(height, metrics.line_height);
    let cols = visible_cell_count(width, metrics.cell_width);
    let total_scrollback = scrollback_len as i64;
    let offset = scroll_offset.clamp(0, total_scrollback);
    let start_line = (total_lines as i64 - rows as i64 - offset).max(0) as usize;

    ViewLayout {
        start_line,
        total_lines,
        rows,
        cols,
    }
}

fn grid_point_at(
    area: &DrawingArea,
    buffer: &TerminalBuffer,
    scroll_offset: i64,
    x: f64,
    y: f64,
    render_cache: &RenderCacheRef,
) -> Option<GridPoint> {
    // 从缓存读取度量；不克隆快照，也不重新分配 Pango layout。
    let metrics = render_cache.borrow().as_ref()?.metrics;
    let layout = view_layout_from_counts(
        area.allocated_width(),
        area.allocated_height(),
        buffer.scrollback_len(),
        buffer.total_line_count(),
        scroll_offset,
        metrics,
    );
    if layout.total_lines == 0 {
        return None;
    }

    let clamped_x = x.clamp(0.0, f64::from(area.allocated_width().saturating_sub(1)));
    let clamped_y = y.clamp(0.0, f64::from(area.allocated_height().saturating_sub(1)));
    let row = (clamped_y / metrics.line_height).floor() as usize;
    let col = (clamped_x / metrics.cell_width).floor() as usize;
    let line = (layout.start_line + row.min(layout.rows.saturating_sub(1)))
        .min(layout.total_lines.saturating_sub(1));

    Some(GridPoint {
        line,
        col: col.min(layout.cols.saturating_sub(1)),
    })
}

fn url_at_point(
    area: &DrawingArea,
    buffer: &TerminalBuffer,
    scroll_offset: i64,
    x: f64,
    y: f64,
    render_cache: &RenderCacheRef,
) -> Option<HoveredUrl> {
    let point = grid_point_at(area, buffer, scroll_offset, x, y, render_cache)?;
    url_at_grid_point(
        buffer,
        scroll_offset,
        area.allocated_width(),
        area.allocated_height(),
        point,
        render_cache,
    )
}

fn url_at_grid_point(
    buffer: &TerminalBuffer,
    scroll_offset: i64,
    width: i32,
    height: i32,
    point: GridPoint,
    render_cache: &RenderCacheRef,
) -> Option<HoveredUrl> {
    let metrics = render_cache.borrow().as_ref()?.metrics;
    let layout = view_layout_from_counts(
        width,
        height,
        buffer.scrollback_len(),
        buffer.total_line_count(),
        scroll_offset,
        metrics,
    );
    if point.line < layout.start_line || point.line >= layout.start_line + layout.rows {
        return None;
    }
    let line = buffer.line_at(point.line)?;
    let rendered = rendered_line_text(line, layout.cols);
    for range in url_ranges(rendered.text.as_str()) {
        let mut start_col = usize::MAX;
        let mut end_col = 0usize;
        let mut contains_point = false;
        for rendered_char in &rendered.chars {
            if rendered_char.byte_start >= range.end || rendered_char.byte_end <= range.start {
                continue;
            }
            start_col = start_col.min(rendered_char.col);
            end_col = end_col.max(rendered_char.col + rendered_char.width);
            if point.col >= rendered_char.col && point.col < rendered_char.col + rendered_char.width
            {
                contains_point = true;
            }
        }
        if contains_point && start_col < end_col {
            return Some(HoveredUrl {
                line: point.line,
                start_col,
                end_col,
                url: range.url,
            });
        }
    }
    None
}

fn extract_selection_text(
    snapshot: &TerminalSnapshot,
    selection: Selection,
    layout: ViewLayout,
) -> String {
    if layout.total_lines == 0 {
        return String::new();
    }

    let all_lines: Vec<&Vec<Cell>> = snapshot
        .scrollback
        .iter()
        .chain(snapshot.lines.iter())
        .collect();
    let (start, end) = selection.normalized();
    let start_line = start.line.min(all_lines.len().saturating_sub(1));
    let end_line = end.line.min(all_lines.len().saturating_sub(1));
    let mut rows = Vec::new();

    for (line_idx, line) in all_lines
        .iter()
        .enumerate()
        .take(end_line + 1)
        .skip(start_line)
    {
        let start_col = if line_idx == start_line { start.col } else { 0 };
        let end_col = if line_idx == end_line {
            end.col.min(line.len().saturating_sub(1))
        } else {
            line.len().saturating_sub(1)
        };

        let text: String = line
            .iter()
            .enumerate()
            .filter(|(col, _)| *col >= start_col && *col <= end_col)
            .filter_map(|(_, cell)| (!cell.is_continuation()).then_some(cell.ch))
            .collect();
        rows.push(text.trim_end_matches(' ').to_string());
    }

    rows.join("\n")
}

fn key_to_bytes(key: gdk::Key, mods: gdk::ModifierType) -> Option<Vec<u8>> {
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);

    // Ctrl + 字母 -> C0 控制码。
    if ctrl && !shift {
        if let Some(ch) = key.to_unicode() {
            let lower = ch.to_ascii_lowercase();
            if lower.is_ascii_alphabetic() {
                let ctrl_byte = (lower as u8) - b'a' + 1;
                return Some(vec![ctrl_byte]);
            }
        }
        // Ctrl+\ -> FS (0x1C)，Ctrl+] -> GS (0x1D)，Ctrl+_ -> US (0x1F)。
        match key {
            gdk::Key::backslash => return Some(vec![0x1C]),
            gdk::Key::bracketright => return Some(vec![0x1B]),
            gdk::Key::minus | gdk::Key::underscore => return Some(vec![0x1F]),
            _ => {}
        }
    }

    // 特殊按键。
    match key {
        gdk::Key::Return => Some(b"\r".to_vec()),
        gdk::Key::KP_Enter => Some(b"\r".to_vec()),
        gdk::Key::BackSpace => Some(vec![0x7F]),
        gdk::Key::Tab => Some(b"\t".to_vec()),
        gdk::Key::Escape => Some(vec![0x1B]),
        gdk::Key::Delete => Some(b"\x1b[3~".to_vec()),
        gdk::Key::Insert => Some(b"\x1b[2~".to_vec()),
        gdk::Key::Home => Some(b"\x1b[H".to_vec()),
        gdk::Key::End => Some(b"\x1b[F".to_vec()),
        gdk::Key::Page_Up => Some(b"\x1b[5~".to_vec()),
        gdk::Key::Page_Down => Some(b"\x1b[6~".to_vec()),
        gdk::Key::Up => Some(b"\x1b[A".to_vec()),
        gdk::Key::Down => Some(b"\x1b[B".to_vec()),
        gdk::Key::Right => Some(b"\x1b[C".to_vec()),
        gdk::Key::Left => Some(b"\x1b[D".to_vec()),
        // 功能键 F1-F12。
        gdk::Key::F1 => Some(b"\x1bOP".to_vec()),
        gdk::Key::F2 => Some(b"\x1bOQ".to_vec()),
        gdk::Key::F3 => Some(b"\x1bOR".to_vec()),
        gdk::Key::F4 => Some(b"\x1bOS".to_vec()),
        gdk::Key::F5 => Some(b"\x1b[15~".to_vec()),
        gdk::Key::F6 => Some(b"\x1b[17~".to_vec()),
        gdk::Key::F7 => Some(b"\x1b[18~".to_vec()),
        gdk::Key::F8 => Some(b"\x1b[19~".to_vec()),
        gdk::Key::F9 => Some(b"\x1b[20~".to_vec()),
        gdk::Key::F10 => Some(b"\x1b[21~".to_vec()),
        gdk::Key::F11 => Some(b"\x1b[23~".to_vec()),
        gdk::Key::F12 => Some(b"\x1b[24~".to_vec()),
        // 普通字符（包括 Shift+字母）。
        _ => key.to_unicode().map(|ch| ch.to_string().into_bytes()),
    }
}

// ── Scroll ────────────────────────────────────────────────────────────────────

fn connect_scroll(
    area: &DrawingArea,
    scroll_offset: Rc<RefCell<i64>>,
    buffer: Rc<RefCell<TerminalBuffer>>,
    font_zoom_handler: FontZoomHandler,
) {
    let controller = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
    let area_for_redraw = area.clone();
    controller.connect_scroll(move |controller, _dx, dy| {
        if controller
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            let steps = if dy < 0.0 {
                1
            } else if dy > 0.0 {
                -1
            } else {
                0
            };
            if steps != 0
                && let Some(handler) = font_zoom_handler.borrow().as_ref()
            {
                handler(steps);
            }
            return glib::Propagation::Stop;
        }

        let scrollback_len = buffer.borrow().scrollback_len() as i64;
        let mut offset = scroll_offset.borrow_mut();
        // dy < 0 表示滚轮向上，进入历史回滚区域。
        if dy < 0.0 {
            *offset = (*offset + SCROLL_LINES).min(scrollback_len);
        } else {
            *offset = (*offset - SCROLL_LINES).max(0);
        }
        area_for_redraw.queue_draw();
        glib::Propagation::Stop
    });
    area.add_controller(controller);
}

#[cfg(test)]
mod tests {
    use shell_core::TerminalSize;
    use shell_terminal::TerminalBuffer;

    use super::*;

    fn cells_from_text(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|ch| Cell {
                ch,
                attrs: Default::default(),
            })
            .collect()
    }

    fn highlights_for_text(text: &str) -> Vec<Option<SemanticHighlight>> {
        let cells = cells_from_text(text);
        semantic_highlights_for_line(&cells, cells.len())
    }

    fn assert_substring_highlighted(
        text: &str,
        highlights: &[Option<SemanticHighlight>],
        needle: &str,
        expected: SemanticHighlight,
    ) {
        let start = text.find(needle).expect("test substring should exist");
        for col in start..start + needle.len() {
            assert_eq!(highlights[col], Some(expected), "column {col} in {needle}");
        }
    }

    #[test]
    fn extracts_selected_text_across_lines() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(8, 2), 8);
        for ch in "hello".chars() {
            buffer.put_char(ch);
        }
        buffer.new_line();
        for ch in "world".chars() {
            buffer.put_char(ch);
        }

        let snapshot = buffer.snapshot();
        let text = extract_selection_text(
            &snapshot,
            Selection {
                anchor: GridPoint { line: 0, col: 1 },
                focus: GridPoint { line: 1, col: 2 },
            },
            view_layout(200, 200, &snapshot, 0, CellMetrics::default()),
        );

        assert_eq!(text, "ello\nwor");
    }

    #[test]
    fn selection_contains_normalizes_bounds() {
        let selection = Selection {
            anchor: GridPoint { line: 3, col: 5 },
            focus: GridPoint { line: 1, col: 2 },
        };

        assert!(selection_contains(selection, 1, 2));
        assert!(selection_contains(selection, 2, 0));
        assert!(selection_contains(selection, 3, 5));
        assert!(!selection_contains(selection, 0, 0));
        assert!(!selection_contains(selection, 3, 6));
    }

    #[test]
    fn extracts_wide_characters_without_continuation_cells() {
        let mut buffer = TerminalBuffer::new(TerminalSize::new(8, 2), 8);
        buffer.put_char('中');
        buffer.put_char('a');

        let snapshot = buffer.snapshot();
        let text = extract_selection_text(
            &snapshot,
            Selection {
                anchor: GridPoint { line: 0, col: 0 },
                focus: GridPoint { line: 0, col: 2 },
            },
            view_layout(200, 200, &snapshot, 0, CellMetrics::default()),
        );

        assert_eq!(text, "中a");
    }

    #[test]
    fn view_layout_does_not_overestimate_visible_cells() {
        let snapshot = TerminalBuffer::new(TerminalSize::new(10, 3), 8).snapshot();
        let layout = view_layout(
            15,
            17,
            &snapshot,
            0,
            CellMetrics {
                cell_width: 8.0,
                line_height: 18.0,
            },
        );

        assert_eq!(layout.cols, 1);
        assert_eq!(layout.rows, 1);
    }

    #[test]
    fn semantic_highlights_urls_and_ip_addresses() {
        let text = "IPv4 address for enp6s18: 192.168.6.111 docs https://ubuntu.com/esm";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "192.168.6.111", ip_highlight());
        assert_substring_highlighted(text, &highlights, "https://ubuntu.com/esm", url_highlight());
    }

    #[test]
    fn url_ranges_trim_trailing_punctuation_and_normalize_www() {
        let ranges = url_ranges("docs: www.example.com/path, mirror https://example.org). ");

        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].url, "https://www.example.com/path");
        assert_eq!(ranges[1].url, "https://example.org");
    }

    #[test]
    fn semantic_highlights_disk_values_and_paths() {
        let text = "tmpfs 792M 1.3M 791M 1% /run/user/1000";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "792M", size_highlight());
        assert_substring_highlighted(text, &highlights, "1%", number_highlight());
        assert_substring_highlighted(text, &highlights, "/run/user/", path_highlight());
        assert_substring_highlighted(text, &highlights, "1000", number_highlight());
    }

    #[test]
    fn semantic_highlights_prompt_commands() {
        let text = "root@ub24:~# df -Th";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "df", command_highlight());
        assert_substring_highlighted(text, &highlights, "-Th", command_option_highlight());
        assert_eq!(highlights[text.find("root").unwrap()], None);
    }

    #[test]
    fn semantic_highlights_process_table_fields() {
        let header = "PID USER PR NI VIRT RES SHR S %CPU %MEM TIME+ COMMAND";
        let header_highlights = highlights_for_text(header);

        assert_substring_highlighted(header, &header_highlights, "PID", table_header_highlight());
        assert_substring_highlighted(
            header,
            &header_highlights,
            "COMMAND",
            table_header_highlight(),
        );

        let row = "1209264 root 20 0 12152 8516 7388 S 9.1 0.1 0:00.02 sshd";
        let row_highlights = highlights_for_text(row);

        assert_substring_highlighted(row, &row_highlights, "S", process_state_highlight());
        assert_substring_highlighted(row, &row_highlights, "9.1", number_highlight());
        assert_substring_highlighted(row, &row_highlights, "sshd", process_highlight());
    }

    #[test]
    fn semantic_highlights_log_levels_http_status_and_env_keys() {
        let text = "ERROR request status=500 method=GET PATH=/api/users --verbose";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "ERROR", error_highlight());
        assert_substring_highlighted(text, &highlights, "500", error_highlight());
        assert_substring_highlighted(text, &highlights, "PATH", env_key_highlight());
        assert_substring_highlighted(text, &highlights, "/api/users", path_highlight());
        assert_substring_highlighted(text, &highlights, "--verbose", command_option_highlight());
    }

    #[test]
    fn semantic_highlights_status_code_classes() {
        let text = "HTTP status 200 then response status 404 and final status 503";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "200", success_highlight());
        assert_substring_highlighted(text, &highlights, "404", warning_highlight());
        assert_substring_highlighted(text, &highlights, "503", error_highlight());
    }

    #[test]
    fn semantic_highlights_network_assignments_and_split_sizes() {
        let text = "root@ub24:~# ifconfig enp6s18: flags=4163 mtu 1500 inet=192.168.6.111 RX bytes 5628785320 (5.6 GB)";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "ifconfig", command_highlight());
        assert_substring_highlighted(text, &highlights, "flags", label_highlight());
        assert_substring_highlighted(text, &highlights, "4163", number_highlight());
        assert_substring_highlighted(text, &highlights, "192.168.6.111", ip_highlight());
        assert_substring_highlighted(text, &highlights, "5.6", size_highlight());
        assert_substring_highlighted(text, &highlights, "GB", size_highlight());
    }

    #[test]
    fn semantic_highlights_more_path_forms() {
        let text = "cp ./target/release/R-shell.exe C:/Temp/R-shell.exe HOME=~/shell";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "cp", command_highlight());
        assert_substring_highlighted(text, &highlights, "./target/release", path_highlight());
        assert_substring_highlighted(text, &highlights, "C:/Temp/R-shell.exe", path_highlight());
        assert_substring_highlighted(text, &highlights, "HOME", env_key_highlight());
        assert_substring_highlighted(text, &highlights, "~/shell", path_highlight());
    }

    #[test]
    fn semantic_highlight_avoids_table_and_process_state_false_positives() {
        let text = "service is running on host S";
        let highlights = highlights_for_text(text);

        assert_eq!(highlights[text.find("on").unwrap()], None);
        assert_eq!(highlights[text.rfind('S').unwrap()], None);
    }

    #[test]
    fn semantic_highlights_dates_times_and_warnings() {
        let text = "Last login: Sat May 9 06:14:42 2026 from 192.168.6.1 restart required";
        let highlights = highlights_for_text(text);

        assert_substring_highlighted(text, &highlights, "Sat", date_highlight());
        assert_substring_highlighted(text, &highlights, "May", date_highlight());
        assert_substring_highlighted(text, &highlights, "06:14:42", number_highlight());
        assert_substring_highlighted(text, &highlights, "192.168.6.1", ip_highlight());
        assert_substring_highlighted(text, &highlights, "restart", warning_highlight());
        assert_substring_highlighted(text, &highlights, "required", warning_highlight());
    }

    #[test]
    fn semantic_highlight_does_not_override_ansi_colored_cells() {
        let cell = Cell {
            ch: '4',
            attrs: shell_terminal::Attrs {
                fg: Color::Indexed(2),
                bg: Color::Default,
                flags: AttrsFlags::empty(),
            },
        };

        assert!(!should_apply_semantic_highlight(&cell, false, false));
        assert!(!should_apply_semantic_highlight(
            &Cell::default(),
            true,
            false
        ));
        assert!(!should_apply_semantic_highlight(
            &Cell::default(),
            false,
            true
        ));
    }

    #[test]
    fn normalize_font_description_keeps_primary_family_and_size() {
        assert_eq!(
            normalize_font_description(
                "Cascadia Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji, Segoe UI Symbol 13"
            ),
            "Cascadia Mono 13"
        );
        assert_eq!(
            normalize_font_description("JetBrains Mono 15"),
            "JetBrains Mono 15"
        );
    }

    #[test]
    fn adjusted_font_description_size_preserves_family_list() {
        assert_eq!(
            adjusted_font_description_size(
                "Cascadia Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji 13",
                1,
            ),
            "Cascadia Mono, Microsoft YaHei UI, SimSun-ExtB, Segoe UI Emoji 14"
        );
    }

    #[test]
    fn adjusted_font_description_size_clamps_to_readable_range() {
        assert_eq!(
            adjusted_font_description_size("Consolas 8", -1),
            "Consolas 8"
        );
        assert_eq!(
            adjusted_font_description_size("Consolas 32", 1),
            "Consolas 32"
        );
    }

    #[test]
    fn adjusted_font_description_size_adds_missing_size() {
        assert_eq!(
            adjusted_font_description_size("JetBrains Mono", 1),
            "JetBrains Mono 14"
        );
    }

    #[test]
    fn focused_cursor_respects_blink_state() {
        assert!(cursor_should_paint(true, true, true));
        assert!(!cursor_should_paint(true, true, false));
    }

    #[test]
    fn unfocused_cursor_stays_visible_when_enabled() {
        assert!(cursor_should_paint(false, true, false));
        assert!(!cursor_should_paint(false, false, true));
    }

    #[test]
    fn rare_cjk_uses_extb_fallback() {
        let font = FontDescription::from_string("Cascadia Mono 13");
        let resolved = fallback_font_description('𠀀', &font).unwrap();
        assert_eq!(resolved.family().as_deref(), Some("SimSun-ExtB"));
    }

    #[test]
    fn cjk_punctuation_uses_yahei_fallback() {
        let font = FontDescription::from_string("Cascadia Mono 13");
        let resolved = fallback_font_description('。', &font).unwrap();
        assert_eq!(resolved.family().as_deref(), Some("Microsoft YaHei UI"));
    }
}
