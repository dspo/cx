//! OMP (Oh My Pi) stats 解析器，从 SQLite 数据库读取聚合数据。
//!
//! OMP 在 `~/.omp/stats.db` 中维护 `messages` 表，记录每次 LLM 调用的用量。
//! 解析时执行 GROUP BY day, model, provider 的聚合查询，产出已聚合的 [`RawEntry`]。

use rusqlite::Connection;
use std::path::Path;

use super::RawEntry;

/// OMP 的 agent 标识。
const AGENT_OMP: &str = "omp";

/// 从 OMP SQLite 数据库读取聚合后的用量数据。
///
/// 执行按 (day, model, provider) 聚合的查询，将每一行转换为一个 [`RawEntry`]。
/// 由于数据已经聚合，`dedup_primary` 为 `None`。
pub(crate) fn parse_omp_db(db_path: &Path) -> Vec<RawEntry> {
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut stmt = match conn.prepare(
        "SELECT
            date(timestamp / 1000, 'unixepoch') AS day,
            model,
            provider,
            SUM(input_tokens)   AS input_tokens,
            SUM(output_tokens)  AS output_tokens,
            SUM(cache_read_tokens)  AS cache_read,
            SUM(cache_write_tokens) AS cache_write
         FROM messages
         GROUP BY day, model, provider
         ORDER BY day DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([], |row| {
        let day: String = row.get(0)?;
        let model: String = row.get(1)?;
        let provider: String = row.get(2)?;
        let input_tokens: u64 = row.get::<_, i64>(3)?.max(0) as u64;
        let output_tokens: u64 = row.get::<_, i64>(4)?.max(0) as u64;
        let cache_read: u64 = row.get::<_, i64>(5)?.max(0) as u64;
        let cache_write: u64 = row.get::<_, i64>(6)?.max(0) as u64;
        Ok(RawEntry {
            agent: AGENT_OMP.to_string(),
            model: format!("{provider}/{model}"),
            date: day,
            input_tokens,
            output_tokens,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_write,
            reasoning_output_tokens: 0,
            dedup_primary: None,
            dedup_secondary: None,
            is_sidechain: false,
        })
    }) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    rows.filter_map(|r| r.ok()).collect()
}