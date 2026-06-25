//! SVG 文档骨架 — canvas 尺寸、header/footer/period tabs、基础绘图 helper。
//!
//! 每个 `*_document` 函数返回 `(prefix, suffix)` 对：
//! - `prefix` = `<svg>` 开标签 + `<style>` + 背景 + header + period tabs（如有）
//! - `suffix` = footer + `</svg>` 关标签
//! - 调用方在 prefix 与 suffix 之间插入图表/表格等内容。

use super::palette;

// ── Overview canvas ──────────────────────────────────────────

pub(super) const OV_WIDTH: u32 = 1200;
pub(super) const OV_HEIGHT: u32 = 900;

/// Overview 内边距。
///
/// top = header(44) + period_tabs(28) + gap(8) = 80
#[allow(dead_code)]
pub(super) struct OvMargin {
    pub(super) top: u32,
    pub(super) bottom: u32,
    pub(super) left: u32,
    pub(super) right: u32,
}

pub(super) const OV_MARGIN: OvMargin = OvMargin {
    top: 80,
    bottom: 36,
    left: 80,
    right: 120,
};

// ── Race canvas ──────────────────────────────────────────────

pub(super) const RACE_WIDTH: u32 = 1200;
pub(super) const RACE_HEIGHT: u32 = 600;

// ── Period tab labels ────────────────────────────────────────

const PERIOD_LABELS: &[&str] = &[
    "Today",
    "Yesterday",
    "Last 7 days",
    "Last 31 days",
    "All time",
];

// ── Header / footer heights ──────────────────────────────────

const HEADER_H: u32 = 44;
const PERIOD_TABS_H: u32 = 28;
pub(super) const FOOTER_H: u32 = 36;
/// X 轴日期标签高度（px）。
pub(super) const X_AXIS_LABEL_H: u32 = 18;
/// 区域间间距（px）。
pub(super) const SECTION_GAP: u32 = 12;

// ── CSS style block ──────────────────────────────────────────

fn style_block() -> String {
    format!(
        "\
  .title {{ font-family: {sans}; font-size: 18px; font-weight: 700; fill: {title}; }}
  .subtitle {{ font-family: {sans}; font-size: 13px; fill: {dim}; }}
  .axis-label {{ font-family: {mono}; font-size: 11px; fill: {axis}; }}
  .data-label {{ font-family: {sans}; font-size: 12px; fill: {text}; }}
  .data-value {{ font-family: {mono}; font-size: 12px; fill: {title}; }}
  .grid-line {{ stroke: {grid}; stroke-width: 0.5; stroke-dasharray: 4,4; }}
  .row-even {{ fill: {striped}; opacity: 0.5; }}
  .legend-label {{ font-family: {sans}; font-size: 12px; fill: {title}; }}
  .tab-inactive {{ font-family: {sans}; font-size: 12px; fill: {inactive}; }}
  .tab-active {{ font-family: {sans}; font-size: 12px; font-weight: 600; fill: {active_text}; }}
  .footer-text {{ font-family: {mono}; font-size: 11px; fill: {dim}; }}
",
        sans = palette::SANS,
        mono = palette::MONO,
        title = palette::TITLE,
        text = palette::TEXT,
        dim = palette::DIM,
        axis = palette::AXIS,
        grid = palette::GRID,
        striped = palette::STRIPED,
        inactive = palette::INACTIVE_TAB,
        active_text = palette::ACTIVE_TAB_TEXT,
    )
}

// ── SVG primitive helpers ────────────────────────────────────

/// 矩形元素，可选圆角和透明度。
pub(super) fn ov_rect(x: u32, y: u32, w: u32, h: u32, fill: &str, opacity: f32) -> String {
    format!(
        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"4\" fill=\"{fill}\" opacity=\"{opacity:.2}\"/>",
        x = x,
        y = y,
        w = w,
        h = h,
        fill = fill,
        opacity = opacity,
    )
}

/// 矩形元素（无圆角，用于全宽背景条）。
fn full_rect(x: u32, y: u32, w: u32, h: u32, fill: &str) -> String {
    format!(
        "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{fill}\"/>",
        x = x,
        y = y,
        w = w,
        h = h,
        fill = fill,
    )
}

/// 文本元素，CSS class + text-anchor。
pub(super) fn ov_text(x: u32, y: u32, content: &str, class: &str, anchor: &str) -> String {
    format!(
        "<text x=\"{x}\" y=\"{y}\" class=\"{class}\" text-anchor=\"{anchor}\">{content}</text>",
        x = x,
        y = y,
        class = class,
        anchor = anchor,
        content = content,
    )
}

/// 直线元素。
pub(super) fn ov_line(x1: u32, y1: u32, x2: u32, y2: u32, stroke: &str, width: f32) -> String {
    format!(
        "<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" stroke=\"{stroke}\" stroke-width=\"{width:.1}\"/>",
        x1 = x1,
        y1 = y1,
        x2 = x2,
        y2 = y2,
        stroke = stroke,
        width = width,
    )
}

// ── Period tabs ──────────────────────────────────────────────

fn period_tabs(active_idx: usize) -> String {
    let tab_w = OV_WIDTH / PERIOD_LABELS.len() as u32;
    let mut svg = String::new();

    // tab 条背景
    svg.push_str(&full_rect(
        0,
        HEADER_H,
        OV_WIDTH,
        PERIOD_TABS_H,
        palette::PERIOD_BG,
    ));
    // 底部分割线
    svg.push_str(&ov_line(
        0,
        HEADER_H + PERIOD_TABS_H,
        OV_WIDTH,
        HEADER_H + PERIOD_TABS_H,
        palette::BORDER,
        1.0,
    ));

    for (i, label) in PERIOD_LABELS.iter().enumerate() {
        let x = i as u32 * tab_w + tab_w / 2;
        let y = HEADER_H + PERIOD_TABS_H / 2 + 4; // +4 approximates baseline shift

        if i == active_idx {
            // active pill: colored background rect + white text
            let pill_x = i as u32 * tab_w + 4;
            let pill_w = tab_w - 8;
            svg.push_str(&ov_rect(
                pill_x,
                HEADER_H + 4,
                pill_w,
                PERIOD_TABS_H - 8,
                palette::ACTIVE_TAB,
                1.0,
            ));
            svg.push_str(&ov_text(x, y, label, "tab-active", "middle"));
        } else {
            svg.push_str(&ov_text(x, y, label, "tab-inactive", "middle"));
        }
    }

    svg
}

// ── Header bar ──────────────────────────────────────────────

fn header_bar(title: &str) -> String {
    let mut svg = String::new();
    // background
    svg.push_str(&full_rect(0, 0, OV_WIDTH, HEADER_H, palette::HEADER_BG));
    // bottom border
    svg.push_str(&ov_line(
        0,
        HEADER_H,
        OV_WIDTH,
        HEADER_H,
        palette::BORDER,
        1.0,
    ));
    // title text (left-aligned)
    svg.push_str(&ov_text(16, HEADER_H / 2 + 5, title, "title", "start"));
    // "cx stats" branding (right-aligned)
    svg.push_str(&ov_text(
        OV_WIDTH - 16,
        HEADER_H / 2 + 5,
        "cx stats",
        "subtitle",
        "end",
    ));
    svg
}

// ── Footer bar ──────────────────────────────────────────────

fn footer_bar(y_offset: u32) -> String {
    let mut svg = String::new();
    let y = y_offset;
    // background
    svg.push_str(&full_rect(0, y, OV_WIDTH, FOOTER_H, palette::FOOTER_BG));
    // top border
    svg.push_str(&ov_line(0, y, OV_WIDTH, y, palette::BORDER, 1.0));
    // period hint text
    let hint = "1 Today  2 Yesterday  3 Last 7 days  4 Last 31 days  5 All time";
    svg.push_str(&ov_text(
        16,
        y + FOOTER_H / 2 + 4,
        hint,
        "footer-text",
        "start",
    ));
    svg
}

// ── Document skeletons ──────────────────────────────────────

/// Overview 文档骨架。
///
/// 返回 `(prefix, suffix)`：
/// - `prefix`: `<svg>` + `<style>` + background + header + period tabs
/// - `suffix`: footer + `</svg>`
/// - 调用方在两者之间插入 chart + table 等内容。
///
/// `active_period_idx` 范围 0–4，对应 Today/Yesterday/7d/31d/All。
pub(super) fn ov_document(
    title: &str,
    period_label: &str,
    active_period_idx: usize,
) -> (String, String) {
    let mut prefix = String::new();

    // SVG open tag
    prefix.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{OV_WIDTH}\" height=\"{OV_HEIGHT}\" viewBox=\"0 0 {OV_WIDTH} {OV_HEIGHT}\">\n",
        OV_WIDTH = OV_WIDTH,
        OV_HEIGHT = OV_HEIGHT,
    ));

    // embedded style
    prefix.push_str("<style>\n");
    prefix.push_str(&style_block());
    prefix.push_str("</style>\n");

    // white background
    prefix.push_str(&full_rect(0, 0, OV_WIDTH, OV_HEIGHT, palette::BG));

    // header bar — use period_label as subtitle context in the title
    let header_title = format!("{title} · {period_label}");
    prefix.push_str(&header_bar(&header_title));

    // period tabs
    prefix.push_str(&period_tabs(active_period_idx));

    // suffix: footer at bottom
    let mut suffix = String::new();
    suffix.push_str(&footer_bar(OV_HEIGHT - FOOTER_H));
    suffix.push_str("</svg>\n");

    (prefix, suffix)
}

/// Race 文档骨架。
///
/// 返回 `(prefix, suffix)`：
/// - `prefix`: `<svg>` + `<style>` + background + title + subtitle
/// - `suffix`: footer + `</svg>`
pub(super) fn race_document(title: &str, subtitle: &str) -> (String, String) {
    let mut prefix = String::new();

    // SVG open tag
    prefix.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{RACE_WIDTH}\" height=\"{RACE_HEIGHT}\" viewBox=\"0 0 {RACE_WIDTH} {RACE_HEIGHT}\">\n",
        RACE_WIDTH = RACE_WIDTH,
        RACE_HEIGHT = RACE_HEIGHT,
    ));

    // embedded style
    prefix.push_str("<style>\n");
    prefix.push_str(&style_block());
    prefix.push_str("</style>\n");

    // white background
    prefix.push_str(&full_rect(0, 0, RACE_WIDTH, RACE_HEIGHT, palette::BG));

    // title line (left-aligned, near top)
    prefix.push_str(&ov_text(16, 24, title, "title", "start"));

    // subtitle line (left-aligned, below title)
    prefix.push_str(&ov_text(16, 44, subtitle, "subtitle", "start"));

    // thin separator under subtitle
    prefix.push_str(&ov_line(16, 52, RACE_WIDTH - 16, 52, palette::BORDER, 1.0));

    // suffix: footer at bottom
    let mut suffix = String::new();
    suffix.push_str(&footer_bar(RACE_HEIGHT - FOOTER_H));
    suffix.push_str("</svg>\n");

    (prefix, suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ov_document_contains_style_and_background() {
        let (prefix, suffix) = ov_document("Token Usage", "Last 7 days", 2);
        assert!(prefix.contains("<svg"));
        assert!(prefix.contains("<style>"));
        assert!(prefix.contains(&format!("width=\"{OV_WIDTH}\"")));
        assert!(prefix.contains(&format!("height=\"{OV_HEIGHT}\"")));
        assert!(prefix.contains(palette::BG));
        assert!(prefix.contains("cx stats"));
        assert!(suffix.contains("</svg>"));
    }

    #[test]
    fn ov_document_active_tab_highlighted() {
        let (prefix, _) = ov_document("Token Usage", "Last 7 days", 2);
        // index 2 = "Last 7 days" should have active pill
        assert!(prefix.contains(palette::ACTIVE_TAB));
        assert!(prefix.contains("tab-active"));
        // inactive tabs should also appear
        assert!(prefix.contains("tab-inactive"));
        assert!(prefix.contains("Today"));
        assert!(prefix.contains("Yesterday"));
    }

    #[test]
    fn race_document_contains_title_subtitle() {
        let (prefix, suffix) = race_document("Model Tokens", "Rolling 7 days in Last 31 days");
        assert!(prefix.contains("<svg"));
        assert!(prefix.contains("<style>"));
        assert!(prefix.contains(&format!("width=\"{RACE_WIDTH}\"")));
        assert!(prefix.contains(&format!("height=\"{RACE_HEIGHT}\"")));
        assert!(prefix.contains("Model Tokens"));
        assert!(prefix.contains("Rolling 7 days"));
        assert!(suffix.contains("</svg>"));
    }

    #[test]
    fn ov_margin_constants() {
        assert_eq!(OV_MARGIN.top, 80);
        assert_eq!(OV_MARGIN.bottom, 36);
        assert_eq!(OV_MARGIN.left, 80);
        assert_eq!(OV_MARGIN.right, 120);
    }

    #[test]
    fn helper_functions_produce_valid_svg_elements() {
        let rect = ov_rect(10, 20, 100, 50, "#ff0000", 0.8);
        assert!(rect.contains("rx=\"4\""));
        assert!(rect.contains("fill=\"#ff0000\""));
        assert!(rect.contains("opacity=\"0.80\""));

        let text = ov_text(50, 100, "hello", "title", "middle");
        assert!(text.contains("class=\"title\""));
        assert!(text.contains(">hello</text>"));

        let line = ov_line(0, 0, 100, 100, "#000", 1.5);
        assert!(line.contains("stroke=\"#000\""));
        assert!(line.contains("stroke-width=\"1.5\""));
    }

    #[test]
    fn period_labels_count() {
        assert_eq!(PERIOD_LABELS.len(), 5);
    }

    #[test]
    fn footer_bar_positioned_at_bottom() {
        let (_prefix, suffix) = ov_document("Test", "All time", 4);
        // footer should be at OV_HEIGHT - FOOTER_H = 864
        let footer_y = OV_HEIGHT - FOOTER_H;
        assert!(suffix.contains(&format!("y=\"{footer_y}\"")));
    }
}
