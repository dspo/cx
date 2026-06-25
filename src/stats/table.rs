//! SVG 模型表格渲染 — Overview 视图下方专业数据表。
//!
//! 用 SVG `<g>` 分组逐行渲染，每行含模型名、占比、占比条形、总量、各 agent 分项。
//! 不复用 TUI (view.rs/tui.rs) 的任何布局逻辑，独立于终端渲染。

use std::collections::HashMap;

use super::format::{format_share, format_tokens};
use super::palette::{BORDER, DIM, MONO, SANS, STRIPED, SVG_COLORS, TEXT, TITLE};
use super::types::UsageTotals;

/// 将模型名映射到 SVG_COLORS 中的颜色，按 sorted 顺序分配。
pub(super) fn assign_colors(sorted: &[(String, UsageTotals)]) -> HashMap<String, String> {
    sorted
        .iter()
        .enumerate()
        .map(|(i, (model, _))| {
            let color = SVG_COLORS
                .get(i)
                .unwrap_or(&SVG_COLORS[SVG_COLORS.len() - 1]);
            (model.clone(), color.to_string())
        })
        .collect()
}

/// 从 totals HashMap + top 模型名列表构建按 total_tokens 降序排列的 (model, UsageTotals) 列表。
pub(super) fn sorted_models(
    top: &[String],
    totals: &HashMap<String, UsageTotals>,
) -> Vec<(String, UsageTotals)> {
    let mut sorted: Vec<(String, UsageTotals)> = top
        .iter()
        .filter_map(|m| totals.get(m).map(|u| (m.clone(), *u)))
        .collect();
    sorted.sort_by(|a, b| b.1.total_tokens().cmp(&a.1.total_tokens()));
    sorted
}

/// 入口函数：接收 top 模型名列表和 totals 映射，渲染完整模型表格。
///
/// 对外暴露给 layout/dispatch 调用的简化接口；内部调用 `render_table` 完成实际渲染。
pub(super) fn model_table(
    top: &[String],
    totals: &HashMap<String, UsageTotals>,
    cells: &HashMap<(String, String), UsageTotals>,
    agents: &[(&str, &str)],
    table_bounds: (f64, f64, f64),
) -> String {
    let sorted = sorted_models(top, totals);
    let color_map = assign_colors(&sorted);
    let total_all: u64 = totals.values().map(|u| u.total_tokens()).sum();
    render_table(&sorted, cells, &color_map, agents, table_bounds, total_all)
}

/// 表格列定义常量。
const ROW_HEIGHT: f64 = 28.0;
const HEADER_HEIGHT: f64 = 24.0;
const COL_GAP: f64 = 12.0;
const MODEL_COL_WIDTH: f64 = 140.0;
const SHARE_TEXT_WIDTH: f64 = 56.0;
const SHARE_BAR_WIDTH: f64 = 80.0;
const TOTAL_COL_WIDTH: f64 = 100.0;
const AGENT_COL_MIN_WIDTH: f64 = 80.0;
const FONT_SIZE_HEADER: f64 = 11.0;
const FONT_SIZE_ROW: f64 = 11.0;
const FONT_SIZE_NAME: f64 = 12.0;
const BAR_HEIGHT: f64 = 8.0;
const BAR_RX: f64 = 2.0;
const ROW_RX: f64 = 4.0;
const PAD_LEFT: f64 = 12.0;

/// 渲染模型表格为 SVG `<g>` 元素。
///
/// # 参数
/// - `sorted`: 模型列表，按 total_tokens 降序排列
/// - `cells`: (agent, model) → UsageTotals 的细粒度映射
/// - `color_map`: model → hex 颜色（来自 SVG_COLORS 分配）
/// - `agents`: MATRIX_AGENTS — (agent_id, display_name)
/// - `table_bounds`: (x, y, width) — 起始坐标和可用宽度
/// - `total_all`: 全局总 token 数，用于计算占比百分比
pub(super) fn render_table(
    sorted: &[(String, UsageTotals)],
    cells: &HashMap<(String, String), UsageTotals>,
    color_map: &HashMap<String, String>,
    agents: &[(&str, &str)],
    table_bounds: (f64, f64, f64),
    total_all: u64,
) -> String {
    let (tx, ty, tw) = table_bounds;

    // 计算固定列宽总和
    let fixed_width = MODEL_COL_WIDTH
        + COL_GAP
        + SHARE_TEXT_WIDTH
        + COL_GAP
        + SHARE_BAR_WIDTH
        + COL_GAP
        + TOTAL_COL_WIDTH;

    // 可用宽度分配给 agent 列
    let remaining = tw - fixed_width - PAD_LEFT;
    let agent_count = agents.len().max(1);
    let agent_col_width = if remaining > 0.0 {
        let w = (remaining - COL_GAP * (agent_count as f64 - 1.0)) / agent_count as f64;
        w.max(AGENT_COL_MIN_WIDTH)
    } else {
        AGENT_COL_MIN_WIDTH
    };

    // 各列的 x 坐标（从左边开始）
    let x_model = tx + PAD_LEFT;
    let x_share_text = x_model + MODEL_COL_WIDTH + COL_GAP;
    let x_share_bar = x_share_text + SHARE_TEXT_WIDTH + COL_GAP;
    let x_total = x_share_bar + SHARE_BAR_WIDTH + COL_GAP;
    // agent 列从 x_total 之后依次排列
    let agent_x: Vec<f64> = agents
        .iter()
        .enumerate()
        .map(|(i, _)| x_total + TOTAL_COL_WIDTH + COL_GAP + i as f64 * (agent_col_width + COL_GAP))
        .collect();

    let mut svg = String::new();

    // ── 表格标题 ──
    svg.push_str(&format!(
        "<text x=\"{x_model:.1}\" y=\"{ty:.1}\" font-family=\"{SANS}\" \
         font-size=\"13\" font-weight=\"600\" fill=\"{TITLE}\">Model Table</text>\n"
    ));

    let header_y = ty + 18.0;

    // ── 表头背景条 ──
    svg.push_str(&format!(
        "<rect x=\"{tx:.1}\" y=\"{header_y:.1}\" width=\"{tw:.1}\" \
         height=\"{HEADER_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{BORDER}\" opacity=\"0.15\"/>\n"
    ));

    let header_text_y = header_y + HEADER_HEIGHT * 0.7;

    // ── 表头文字 ──
    // Model 列
    svg.push_str(&format!(
        "<text x=\"{x_model:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"start\">Model</text>\n"
    ));
    // Share 列
    svg.push_str(&format!(
        "<text x=\"{x_share_bar:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"start\">Share</text>\n"
    ));
    // Total 列
    let x_total_end = x_total + TOTAL_COL_WIDTH;
    svg.push_str(&format!(
        "<text x=\"{x_total_end:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"end\">Total</text>\n"
    ));
    // Agent 列表头
    for (i, (_, display_name)) in agents.iter().enumerate() {
        let ax = agent_x[i] + agent_col_width;
        svg.push_str(&format!(
            "<text x=\"{ax:.1}\" y=\"{header_text_y:.1}\" \
             font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
             font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"end\">{display_name}</text>\n"
        ));
    }

    let data_start_y = header_y + HEADER_HEIGHT + 4.0;

    // ── 数据行 ──
    for (row_idx, (model_name, usage)) in sorted.iter().enumerate() {
        let row_y = data_start_y + row_idx as f64 * ROW_HEIGHT;
        let row_mid_y = row_y + ROW_HEIGHT * 0.55;
        let color = color_map
            .get(model_name)
            .map(|s| s.as_str())
            .unwrap_or(SVG_COLORS[0]);

        // ── 条纹行背景 ──
        if row_idx % 2 == 1 {
            svg.push_str(&format!(
                "<rect x=\"{tx:.1}\" y=\"{row_y:.1}\" width=\"{tw:.1}\" \
                 height=\"{ROW_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{STRIPED}\" opacity=\"0.5\"/>\n"
            ));
        }

        // ── 模型颜色微底纹 ──
        svg.push_str(&format!(
            "<rect x=\"{tx:.1}\" y=\"{row_y:.1}\" width=\"{tw:.1}\" \
             height=\"{ROW_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{color}\" opacity=\"0.08\"/>\n"
        ));

        // ── 模型名 ──
        svg.push_str(&format!(
            "<text x=\"{x_model:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{SANS}\" font-size=\"{FONT_SIZE_NAME}\" \
             font-weight=\"600\" fill=\"{TITLE}\" text-anchor=\"start\">{model_name}</text>\n"
        ));

        // ── 占比 ──
        let share_pct = if total_all > 0 {
            usage.total_tokens() as f64 / total_all as f64 * 100.0
        } else {
            0.0
        };
        let share_text = format_share(share_pct);
        svg.push_str(&format!(
            "<text x=\"{x_share_bar:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
             fill=\"{DIM}\" text-anchor=\"start\">{share_text}</text>\n"
        ));

        // ── 占比条形图 ──
        // 条形在占比文字下方
        let bar_y = row_mid_y + 4.0;
        // 背景：条纹色
        svg.push_str(&format!(
            "<rect x=\"{x_share_bar:.1}\" y=\"{bar_y:.1}\" width=\"{SHARE_BAR_WIDTH:.1}\" \
             height=\"{BAR_HEIGHT:.1}\" rx=\"{BAR_RX}\" fill=\"{STRIPED}\"/>\n"
        ));
        // 前景：模型色，宽度按占比比例
        if share_pct > 0.0 {
            let bar_fill_width = (share_pct / 100.0 * SHARE_BAR_WIDTH).min(SHARE_BAR_WIDTH);
            svg.push_str(&format!(
                "<rect x=\"{x_share_bar:.1}\" y=\"{bar_y:.1}\" width=\"{bar_fill_width:.1}\" \
                 height=\"{BAR_HEIGHT:.1}\" rx=\"{BAR_RX}\" fill=\"{color}\" opacity=\"0.4\"/>\n"
            ));
        }

        // ── Total tokens ──
        let total_text = format!(
            "↑{} ↓{}",
            format_tokens(usage.in_tokens),
            format_tokens(usage.out_tokens)
        );
        let x_total_end = x_total + TOTAL_COL_WIDTH;
        svg.push_str(&format!(
            "<text x=\"{x_total_end:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
             fill=\"{TEXT}\" text-anchor=\"end\">{total_text}</text>\n"
        ));

        // ── 各 Agent 分项 ──
        for (i, (agent_id, _)) in agents.iter().enumerate() {
            let ax = agent_x[i] + agent_col_width;
            let agent_usage = cells.get(&(agent_id.to_string(), model_name.clone()));
            let cell_text = match agent_usage {
                Some(u) if u.total_tokens() > 0 => {
                    format!(
                        "↑{} ↓{}",
                        format_tokens(u.in_tokens),
                        format_tokens(u.out_tokens)
                    )
                }
                _ => "—".to_string(),
            };
            svg.push_str(&format!(
                "<text x=\"{ax:.1}\" y=\"{row_mid_y:.1}\" \
                 font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
                 fill=\"{TEXT}\" text-anchor=\"end\">{cell_text}</text>\n"
            ));
        }
    }

    // ── 底部分隔线 ──
    let bottom_y = data_start_y + sorted.len() as f64 * ROW_HEIGHT + 2.0;
    let x2 = tx + tw;
    svg.push_str(&format!(
        "<line x1=\"{tx:.1}\" y1=\"{bottom_y:.1}\" x2=\"{x2:.1}\" y2=\"{bottom_y:.1}\" \
         stroke=\"{BORDER}\" stroke-width=\"0.5\"/>\n"
    ));

    svg
}

/// 计算表格所需总高度（像素）。
///
/// Computes the expected table height in pixels for a given number of model rows.
#[allow(dead_code)]
pub(super) fn table_height(row_count: usize) -> f64 {
    18.0 + HEADER_HEIGHT + 4.0 + row_count as f64 * ROW_HEIGHT + 2.0 + 4.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::MATRIX_AGENTS;

    /// 空数据 → 只包含标题和表头，没有数据行。
    #[test]
    fn render_empty_table() {
        let sorted: Vec<(String, UsageTotals)> = vec![];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents = MATRIX_AGENTS;
        let bounds = (0.0, 0.0, 1200.0);
        let svg = render_table(&sorted, &cells, &color_map, agents, bounds, 0);
        // 包含标题、表头背景、表头文字、底部分隔线
        assert!(svg.contains("Model Table"));
        assert!(svg.contains("Model</text>"));
        assert!(svg.contains("Share</text>"));
        assert!(svg.contains("Total</text>"));
        // 无数据行
        assert!(!svg.contains("font-weight=\"600\" fill=\"#4c4f69\" text-anchor=\"start\">"));
    }

    /// 单模型单行 → 正常渲染模型名、占比、条形、总量、各 agent 分项。
    #[test]
    fn render_single_model_row() {
        let usage = UsageTotals {
            in_tokens: 234_000_000,
            total_tokens: 235_000_000,
            out_tokens: 1_000_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let sorted = vec![("glm-5".to_string(), usage)];
        let cells: HashMap<(String, String), UsageTotals> = HashMap::from([(
            ("claude".to_string(), "glm-5".to_string()),
            UsageTotals {
                in_tokens: 234_000_000,
                total_tokens: 234_000_000,
                out_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )]);
        let color_map = HashMap::from([("glm-5".to_string(), "#4e79a7".to_string())]);
        let agents = MATRIX_AGENTS;
        let bounds = (0.0, 0.0, 1200.0);
        let svg = render_table(&sorted, &cells, &color_map, agents, bounds, 235_000_000);

        // 模型名
        assert!(svg.contains(">glm-5</text>"));
        // 占比文字
        assert!(svg.contains(">100.0%</text>"));
        // 占比条形有前景填充
        assert!(svg.contains("opacity=\"0.4\""));
        // Total
        assert!(svg.contains(">↑234m ↓1m</text>"));
        // Claude Code 列有值
        assert!(svg.contains(">↑234m ↓0</text>"));
        // 其他 agent 列为 "—"
        assert!(svg.contains(">—</text>"));
        // 颜色底纹 opacity 0.08
        assert!(svg.contains("opacity=\"0.08\""));
    }

    /// 奇数行有条纹背景。
    #[test]
    fn striped_row_on_odd_index() {
        let usage = UsageTotals {
            in_tokens: 100,
            total_tokens: 120,
            out_tokens: 20,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let sorted = vec![
            ("a".to_string(), usage),
            ("b".to_string(), usage),
            ("c".to_string(), usage),
        ];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents = MATRIX_AGENTS;
        let bounds = (0.0, 0.0, 1200.0);
        let svg = render_table(&sorted, &cells, &color_map, agents, bounds, 360);

        // 行 0 (even) — 无条纹
        // 行 1 (odd) — 有条纹
        // 行 2 (even) — 无条纹
        let striped_count = svg.matches("fill=\"#eff1f5\" opacity=\"0.5\"").count();
        assert_eq!(striped_count, 1);
    }

    /// table_height 计算正确。
    #[test]
    fn table_height_calculation() {
        // 0 行: 18 + 24 + 4 + 0 + 2 + 4 = 52
        assert_eq!(table_height(0), 52.0);
        // 5 行: 18 + 24 + 4 + 5*28 + 2 + 4 = 192
        assert_eq!(table_height(5), 192.0);
        // 10 行: 18 + 24 + 4 + 280 + 2 + 4 = 332
        assert_eq!(table_height(10), 332.0);
    }

    /// 占比 0 → 条形前景不渲染，文字为 "0.00%"。
    #[test]
    fn zero_share_no_bar_fill() {
        let usage = UsageTotals {
            in_tokens: 0,
            total_tokens: 0,
            out_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let sorted = vec![("empty".to_string(), usage)];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents = MATRIX_AGENTS;
        let bounds = (0.0, 0.0, 1200.0);
        let total_all = 1_000_000;
        let svg = render_table(&sorted, &cells, &color_map, agents, bounds, total_all);

        assert!(svg.contains(">0.00%</text>"));
        // 条形前景 rect 不应出现（share_pct == 0 → 不渲染）
        // 背景条形仍然出现
        assert!(svg.contains("fill=\"#eff1f5\""));
        // 没有 opacity=0.4 的前景条形（因为 share_pct == 0 不渲染前景）
        let fill_bar_count = svg.matches("opacity=\"0.4\"").count();
        assert_eq!(fill_bar_count, 0);
    }

    /// 非零占比的小模型 → 条形前景宽度 < 全宽。
    #[test]
    fn small_share_proportional_bar() {
        let usage = UsageTotals {
            in_tokens: 10_000,
            total_tokens: 10_000,
            out_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let sorted = vec![("tiny".to_string(), usage)];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents = MATRIX_AGENTS;
        let bounds = (0.0, 0.0, 1200.0);
        let total_all = 1_000_000;
        let svg = render_table(&sorted, &cells, &color_map, agents, bounds, total_all);

        // 1% share → bar width = 0.01 * 80 = 0.8
        assert!(svg.contains("width=\"0.8\""));
        assert!(svg.contains("opacity=\"0.4\""));
    }
}
