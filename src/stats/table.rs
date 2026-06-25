//! SVG 模型表格渲染 — Overview 视图下方专业数据表。
//!
//! 用 SVG `<g>` 分组逐行渲染，每行含模型名、占比条形（在 Model 列下方）、
//! 占比百分比、总量、各 agent 分项。
//! 不复用 TUI (view.rs/tui.rs) 的任何布局逻辑，独立于终端渲染。
//!
//! 与 TUI 一致的设计：
//! - Share 占比条形在 Model 列下方（而非单独 Share 列下）
//! - 条形宽度按 Total tokens / max_total_tokens 归一化（最大模型填满 Model 列）
//! - Share 列仅显示百分比文字

use std::collections::HashMap;

use super::format::{format_share, format_tokens};
use super::palette::{BORDER, DIM, MONO, SANS, STRIPED, SVG_COLORS, TEXT, TITLE};
use super::types::UsageTotals;

/// 将模型名映射到 SVG_COLORS 中的颜色，按 sorted 顺序分配。
pub(super) fn assign_colors(sorted: &[(String, UsageTotals)]) -> HashMap<String, String> {
    sorted
        .iter()
        .enumerate()
        .map(|(i, (model, _))| (model.clone(), SVG_COLORS[i % SVG_COLORS.len()].to_string()))
        .collect()
}

/// 从 totals HashMap + top 模型名列表构建按 total_tokens 降序排列的 (model, UsageTotals) 列表。
pub(super) fn sorted_models(
    top: &[String],
    totals: &HashMap<String, UsageTotals>,
) -> Vec<(String, UsageTotals)> {
    let mut v: Vec<(String, UsageTotals)> = top
        .iter()
        .filter_map(|model| {
            totals
                .get(model)
                .filter(|usage| usage.total_tokens() > 0)
                .map(|usage| (model.clone(), *usage))
        })
        .collect();
    v.sort_by(|a, b| {
        b.1.total_tokens()
            .cmp(&a.1.total_tokens())
            .then_with(|| a.0.cmp(&b.0))
    });
    v
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
const COL_GAP: f64 = 14.0;
const MODEL_COL_WIDTH: f64 = 220.0;
const SHARE_COL_WIDTH: f64 = 56.0;
const TOTAL_COL_WIDTH: f64 = 100.0;
const AGENT_COL_MIN_WIDTH: f64 = 80.0;
const FONT_SIZE_HEADER: f64 = 11.0;
const FONT_SIZE_ROW: f64 = 11.0;
const FONT_SIZE_NAME: f64 = 12.0;
const BAR_HEIGHT: f64 = 8.0;
const BAR_RX: f64 = 2.0;
const ROW_RX: f64 = 4.0;
const PAD_LEFT: f64 = 12.0;
/// 表格右边留出的视觉 padding（px）——确保背景矩形覆盖最后一列文字。
const TABLE_RIGHT_PAD: f64 = 16.0;

/// 渲染模型表格为 SVG `<g>` 元素。
///
/// # 参数
/// - `sorted`: 模型列表，按 total_tokens 降序排列
/// - `cells`: (agent, model) → UsageTotals 的细粒度映射
/// - `color_map`: model → hex 颜色（来自 SVG_COLORS 分配）
/// - `agents`: MATRIX_AGENTS — (agent_id, display_name)
/// - `table_bounds`: (x, y, available_width) — 起始坐标和可用宽度
/// - `total_all`: 全局总 token 数，用于计算占比百分比
pub(super) fn render_table(
    sorted: &[(String, UsageTotals)],
    cells: &HashMap<(String, String), UsageTotals>,
    color_map: &HashMap<String, String>,
    agents: &[(&str, &str)],
    table_bounds: (f64, f64, f64),
    total_all: u64,
) -> String {
    let (tx, ty, available_w) = table_bounds;

    // ── 布局计算 ────────────────────────────────────────
    // 固定列：Model | Share | Total
    let fixed_width = MODEL_COL_WIDTH + COL_GAP + SHARE_COL_WIDTH + COL_GAP + TOTAL_COL_WIDTH;

    // agent 列宽度：均匀分配剩余空间
    let remaining = available_w - fixed_width - PAD_LEFT - TABLE_RIGHT_PAD;
    let agent_count = agents.len().max(1);
    let agent_col_width = if remaining > 0.0 {
        let w = (remaining - COL_GAP * (agent_count as f64 - 1.0)) / agent_count as f64;
        w.max(AGENT_COL_MIN_WIDTH)
    } else {
        AGENT_COL_MIN_WIDTH
    };

    // 实际表格宽度 = 所有列占据的总宽度（用于背景矩形）
    let actual_table_w =
        fixed_width + PAD_LEFT + TABLE_RIGHT_PAD + agent_count as f64 * (agent_col_width + COL_GAP)
            - COL_GAP;

    // 各列 x 坐标
    let x_model = tx + PAD_LEFT;
    let x_share = x_model + MODEL_COL_WIDTH + COL_GAP;
    let x_total = x_share + SHARE_COL_WIDTH + COL_GAP;
    let x_total_end = x_total + TOTAL_COL_WIDTH;
    let agent_x: Vec<f64> = agents
        .iter()
        .enumerate()
        .map(|(i, _)| x_total_end + COL_GAP + i as f64 * (agent_col_width + COL_GAP))
        .collect();
    // 每个 agent 列文字右对齐到列右端
    let agent_text_x: Vec<f64> = agent_x.iter().map(|x| x + agent_col_width).collect();

    // ── 条形图归一化基准 ──────────────────────────────
    // 与 TUI 一致：条形宽度按 Total tokens / max_total_tokens 归一化
    let max_total: u64 = sorted
        .iter()
        .map(|(_, usage)| usage.total_tokens())
        .max()
        .unwrap_or(0);

    let mut svg = String::new();

    // ── 表格标题 ──
    svg.push_str(&format!(
        "<text x=\"{x_model:.1}\" y=\"{ty:.1}\" font-family=\"{SANS}\" \
         font-size=\"13\" font-weight=\"600\" fill=\"{TITLE}\">Model Table</text>\n"
    ));

    let header_y = ty + 18.0;

    // ── 表头背景条 ──（覆盖实际宽度）
    svg.push_str(&format!(
        "<rect x=\"{tx:.1}\" y=\"{header_y:.1}\" width=\"{actual_table_w:.1}\" \
         height=\"{HEADER_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{BORDER}\" opacity=\"0.15\"/>\n"
    ));

    let header_text_y = header_y + HEADER_HEIGHT * 0.7;

    // ── 表头文字 ──
    // Model
    svg.push_str(&format!(
        "<text x=\"{x_model:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"start\">Model</text>\n"
    ));
    // Share
    svg.push_str(&format!(
        "<text x=\"{x_share:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"start\">Share</text>\n"
    ));
    // Total
    svg.push_str(&format!(
        "<text x=\"{x_total_end:.1}\" y=\"{header_text_y:.1}\" \
         font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
         font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"end\">Total</text>\n"
    ));
    // Agent headers
    for (i, (_, display_name)) in agents.iter().enumerate() {
        svg.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{header_text_y:.1}\" \
             font-family=\"{SANS}\" font-size=\"{FONT_SIZE_HEADER}\" \
             font-weight=\"600\" fill=\"{DIM}\" text-anchor=\"end\">{display_name}</text>\n",
            agent_text_x[i]
        ));
    }

    let data_start_y = header_y + HEADER_HEIGHT + 4.0;

    // ── 数据行 ──
    for (row_idx, (model_name, usage)) in sorted.iter().enumerate() {
        let row_y = data_start_y + row_idx as f64 * ROW_HEIGHT;
        let row_mid_y = row_y + ROW_HEIGHT * 0.45;
        let bar_y = row_mid_y + FONT_SIZE_NAME + 2.0;
        let color = color_map
            .get(model_name)
            .map(|s| s.as_str())
            .unwrap_or(SVG_COLORS[0]);

        // ── 条纹行背景 ──（覆盖实际宽度）
        if row_idx % 2 == 1 {
            svg.push_str(&format!(
                "<rect x=\"{tx:.1}\" y=\"{row_y:.1}\" width=\"{actual_table_w:.1}\" \
                 height=\"{ROW_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{STRIPED}\" opacity=\"0.5\"/>\n"
            ));
        }

        // ── 模型颜色微底纹 ──（覆盖实际宽度）
        svg.push_str(&format!(
            "<rect x=\"{tx:.1}\" y=\"{row_y:.1}\" width=\"{actual_table_w:.1}\" \
             height=\"{ROW_HEIGHT:.1}\" rx=\"{ROW_RX}\" fill=\"{color}\" opacity=\"0.08\"/>\n"
        ));

        // ── 模型名 ──
        svg.push_str(&format!(
            "<text x=\"{x_model:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{SANS}\" font-size=\"{FONT_SIZE_NAME}\" \
             font-weight=\"600\" fill=\"{TITLE}\" text-anchor=\"start\">{model_name}</text>\n"
        ));

        // ── 占比条形图 ──（在 Model 列下方，与 TUI 一致）
        // 条形宽度按 Total tokens / max_total 归一化（最大模型填满 Model 列）
        let ratio = if max_total > 0 {
            usage.total_tokens() as f64 / max_total as f64
        } else {
            0.0
        };
        // 背景：条纹色
        svg.push_str(&format!(
            "<rect x=\"{x_model:.1}\" y=\"{bar_y:.1}\" width=\"{MODEL_COL_WIDTH:.1}\" \
             height=\"{BAR_HEIGHT:.1}\" rx=\"{BAR_RX}\" fill=\"{STRIPED}\"/>\n"
        ));
        // 前景：模型色，宽度按归一化比例
        if ratio > 0.0 {
            let bar_fill_w = (ratio * MODEL_COL_WIDTH).min(MODEL_COL_WIDTH);
            svg.push_str(&format!(
                "<rect x=\"{x_model:.1}\" y=\"{bar_y:.1}\" width=\"{bar_fill_w:.1}\" \
                 height=\"{BAR_HEIGHT:.1}\" rx=\"{BAR_RX}\" fill=\"{color}\" opacity=\"0.4\"/>\n"
            ));
        }

        // ── 占比百分比 ──
        let share_pct = if total_all > 0 {
            usage.total_tokens() as f64 / total_all as f64 * 100.0
        } else {
            0.0
        };
        let share_text = format_share(share_pct);
        svg.push_str(&format!(
            "<text x=\"{x_share:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
             fill=\"{DIM}\" text-anchor=\"start\">{share_text}</text>\n"
        ));

        // ── Total tokens ──
        let total_text = format!(
            "↑{} ↓{}",
            format_tokens(usage.in_tokens),
            format_tokens(usage.out_tokens)
        );
        svg.push_str(&format!(
            "<text x=\"{x_total_end:.1}\" y=\"{row_mid_y:.1}\" \
             font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
             fill=\"{TEXT}\" text-anchor=\"end\">{total_text}</text>\n"
        ));

        // ── 各 Agent 分项 ──
        for (i, (agent_id, _)) in agents.iter().enumerate() {
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
                "<text x=\"{:.1}\" y=\"{row_mid_y:.1}\" \
                 font-family=\"{MONO}\" font-size=\"{FONT_SIZE_ROW}\" \
                 fill=\"{TEXT}\" text-anchor=\"end\">{cell_text}</text>\n",
                agent_text_x[i]
            ));
        }
    }

    // ── 底部分隔线 ──
    let bottom_y = data_start_y + sorted.len() as f64 * ROW_HEIGHT + 2.0;
    svg.push_str(&format!(
        "<line x1=\"{tx:.1}\" y1=\"{bottom_y:.1}\" x2=\"{:.1}\" y2=\"{bottom_y:.1}\" \
         stroke=\"{BORDER}\" stroke-width=\"0.5\"/>\n",
        tx + actual_table_w
    ));

    svg
}

/// 计算表格所需总高度（像素）。
///
/// Computes the expected table height in pixels for a given number of model rows.
pub(super) fn table_height(row_count: usize) -> f64 {
    18.0 + HEADER_HEIGHT + 4.0 + row_count as f64 * ROW_HEIGHT + 2.0 + 4.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assign_colors_maps_models() {
        let sorted = vec![
            ("glm-5.2".to_string(), UsageTotals::default()),
            ("gpt-5.4".to_string(), UsageTotals::default()),
        ];
        let map = assign_colors(&sorted);
        assert_eq!(map["glm-5.2"], SVG_COLORS[0]);
        assert_eq!(map["gpt-5.4"], SVG_COLORS[1]);
    }

    #[test]
    fn sorted_models_orders_by_total_desc() {
        let totals: HashMap<String, UsageTotals> = [
            (
                "a".to_string(),
                UsageTotals {
                    in_tokens: 80,
                    out_tokens: 20,
                    ..Default::default()
                },
            ),
            (
                "b".to_string(),
                UsageTotals {
                    in_tokens: 400,
                    out_tokens: 100,
                    ..Default::default()
                },
            ),
            (
                "c".to_string(),
                UsageTotals {
                    ..Default::default()
                },
            ),
        ]
        .into_iter()
        .collect();
        let top = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let sorted = sorted_models(&top, &totals);
        assert_eq!(sorted.len(), 2); // c excluded (total=0)
        assert_eq!(sorted[0].0, "b"); // 500 > 100
        assert_eq!(sorted[1].0, "a");
    }

    #[test]
    fn render_table_basic_structure() {
        let sorted = vec![(
            "glm-5.2".to_string(),
            UsageTotals {
                in_tokens: 100,
                total_tokens: 100,
                out_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            100,
        );
        assert!(svg.contains("Model Table"));
        assert!(svg.contains("Model"));
        assert!(svg.contains("Share"));
        assert!(svg.contains("Total"));
        assert!(svg.contains("glm-5.2"));
    }

    #[test]
    fn render_table_with_agents() {
        let sorted = vec![(
            "glm-5.2".to_string(),
            UsageTotals {
                in_tokens: 200,
                total_tokens: 200,
                out_tokens: 80,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )];
        let cells: HashMap<(String, String), UsageTotals> = [(
            ("claude".to_string(), "glm-5.2".to_string()),
            UsageTotals {
                in_tokens: 200,
                total_tokens: 200,
                out_tokens: 80,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )]
        .into_iter()
        .collect();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![("claude", "Claude Code")];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            200,
        );
        assert!(svg.contains("Claude Code"));
        assert!(svg.contains("↑200 ↓80"));
    }

    #[test]
    fn render_table_striped_rows_odd() {
        let sorted = vec![
            (
                "a".to_string(),
                UsageTotals {
                    in_tokens: 80,
                    out_tokens: 20,
                    ..Default::default()
                },
            ),
            (
                "b".to_string(),
                UsageTotals {
                    in_tokens: 40,
                    out_tokens: 10,
                    ..Default::default()
                },
            ),
        ];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            150,
        );
        // Row 1 (odd) should have striped background
        assert!(svg.contains(&format!("fill=\"{STRIPED}\" opacity=\"0.5\"")));
    }

    #[test]
    fn share_bar_under_model_column() {
        let sorted = vec![(
            "glm-5.2".to_string(),
            UsageTotals {
                in_tokens: 100,
                total_tokens: 100,
                out_tokens: 50,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },
        )];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            100,
        );

        // Bar should be at x_model position (under Model column), not at a separate share bar position
        let x_model = 80.0 + PAD_LEFT; // 92.0
        // Bar background rect should start at x_model
        assert!(svg.contains(&format!("x=\"{x_model:.1}\"")));
        // Bar background width should be MODEL_COL_WIDTH
        assert!(svg.contains(&format!("width=\"{MODEL_COL_WIDTH:.1}\"")));
    }

    #[test]
    fn bar_normalized_by_max_total() {
        let sorted = vec![
            (
                "a".to_string(),
                UsageTotals {
                    in_tokens: 160,
                    out_tokens: 40,
                    ..Default::default()
                },
            ),
            (
                "b".to_string(),
                UsageTotals {
                    in_tokens: 80,
                    out_tokens: 20,
                    ..Default::default()
                },
            ),
        ];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            300,
        );

        // Model "a" has ratio=1.0 → bar fills entire MODEL_COL_WIDTH
        assert!(svg.contains(&format!("width=\"{MODEL_COL_WIDTH:.1}\"")));
        // Model "b" has ratio=0.5 → bar is half of MODEL_COL_WIDTH
        let half_bar = MODEL_COL_WIDTH * 0.5;
        assert!(svg.contains(&format!("width=\"{half_bar:.1}\"")));
    }

    #[test]
    fn background_rects_cover_actual_table_width() {
        let sorted = vec![(
            "a".to_string(),
            UsageTotals {
                in_tokens: 80,
                out_tokens: 20,
                ..Default::default()
            },
        )];
        let cells = HashMap::new();
        let color_map = HashMap::new();
        let agents: Vec<(&str, &str)> = vec![("claude", "Claude Code"), ("codex", "Codex")];
        let svg = render_table(
            &sorted,
            &cells,
            &color_map,
            &agents,
            (80.0, 500.0, 1000.0),
            100,
        );

        // Background rects should use actual_table_w (not available_w)
        // actual_table_w = fixed_width + PAD_LEFT + TABLE_RIGHT_PAD + 2*(agent_col_width + COL_GAP) - COL_GAP
        // = (220+14+56+14+100) + 12 + 16 + 2*(w+14) - 14
        // At minimum agent_col_width=80: actual_table_w = 404 + 28 + 2*94 - 14 = 516
        assert!(svg.contains("width=\"")); // has width attribute
        // The width should be > 100 (the available_w param), because we compute actual width
        let width_str = svg
            .lines()
            .find(|l| l.contains("opacity=\"0.15\"") && l.contains("rect"))
            .unwrap();
        let width_val: f64 = width_str
            .split("width=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(width_val > 400.0, "actual_table_w should cover all columns");
    }

    #[test]
    fn table_height_calculation() {
        assert_eq!(table_height(0), 18.0 + 24.0 + 4.0 + 2.0 + 4.0);
        assert_eq!(table_height(5), 18.0 + 24.0 + 4.0 + 5.0 * 28.0 + 2.0 + 4.0);
    }

    #[test]
    fn model_col_width_contains_bar() {
        // MODEL_COL_WIDTH must be wide enough for typical model names + bar below
        assert!(
            MODEL_COL_WIDTH >= 140.0,
            "Model column should be at least 140px wide"
        );
    }
}
