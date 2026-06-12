//! SQLite 持久化用量缓存（~/.config/cx/cx.db）。
//!
//! 替代旧的 JSON 文件缓存（~/.local/share/cx/stats-cache.json），
//! 减少本地文件系统 IO；即便 agent 卸载后数据目录被清除，
//! cx 仍保留其 tokens 数据。

use anyhow::{Context, Result};
use dirs::home_dir;
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

use super::parser::RawEntry;
use super::types::UsageRecord;

pub(super) const DB_VERSION: u32 = 1;

pub(super) fn db_path() -> Result<PathBuf> {
    let home = home_dir().context("无法解析用户主目录")?;
    Ok(home.join(".config/cx/cx.db"))
}

pub(super) fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建数据库目录失败: {}", parent.display()))?;
    }
    let conn = Connection::open(path)
        .with_context(|| format!("打开数据库失败: {}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    Ok(conn)
}

pub(super) fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS scanned_files (
            path       TEXT PRIMARY KEY,
            mtime_secs INTEGER NOT NULL,
            size       INTEGER NOT NULL,
            raw_json   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS usage_records (
            agent      TEXT NOT NULL,
            model      TEXT NOT NULL,
            date       TEXT NOT NULL,
            in_tokens  INTEGER NOT NULL DEFAULT 0,
            out_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_input_tokens  INTEGER NOT NULL DEFAULT 0,
            cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (agent, model, date)
        );",
    )?;

    let current_version: u32 = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if current_version < DB_VERSION {
        // 首次建库或版本升级时重建 usage_records（由上层重新聚合填充）。
        conn.execute("DELETE FROM usage_records", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![DB_VERSION.to_string()],
        )?;
    }

    Ok(())
}

/// 从 DB 加载所有聚合后的 UsageRecord。
///
/// 当前 `run_stats` 每次都重新聚合写入，此函数仅用于测试验证写入正确性。
#[cfg(test)]
pub(super) fn load_aggregated(conn: &Connection) -> Result<Vec<UsageRecord>> {
    let mut stmt = conn.prepare(
        "SELECT agent, model, date, in_tokens, out_tokens,
                cache_read_input_tokens, cache_creation_input_tokens
         FROM usage_records",
    )?;

    let rows = stmt.query_map([], |row| {
        let in_tokens: u64 = row.get(3)?;
        let out_tokens: u64 = row.get(4)?;
        Ok(UsageRecord {
            agent: row.get(0)?,
            model: row.get(1)?,
            date: row.get(2)?,
            in_tokens,
            total_tokens: in_tokens + out_tokens,
            out_tokens,
            cache_read_input_tokens: row.get(5)?,
            cache_creation_input_tokens: row.get(6)?,
        })
    })?;

    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// 检查文件是否未变化（mtime + size 匹配）。
pub(super) fn file_unchanged(conn: &Connection, path: &str, mtime: u64, size: u64) -> bool {
    conn.query_row(
        "SELECT mtime_secs, size FROM scanned_files WHERE path = ?1",
        params![path],
        |row| {
            let cached_mtime: u64 = row.get(0)?;
            let cached_size: u64 = row.get(1)?;
            Ok(cached_mtime == mtime && cached_size == size)
        },
    )
    .unwrap_or(false)
}

/// 获取某文件的缓存 raw entries。
pub(super) fn get_cached_raw(conn: &Connection, path: &str) -> Result<Vec<RawEntry>> {
    let json: String = conn.query_row(
        "SELECT raw_json FROM scanned_files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    Ok(serde_json::from_str(&json).unwrap_or_default())
}

/// 存储文件的 raw entries（upsert）。
pub(super) fn store_file_entries(
    conn: &Connection,
    path: &str,
    mtime: u64,
    size: u64,
    entries: &[RawEntry],
) -> Result<()> {
    let json = serde_json::to_string(entries)?;
    conn.execute(
        "INSERT OR REPLACE INTO scanned_files (path, mtime_secs, size, raw_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![path, mtime, size, json],
    )?;
    Ok(())
}

/// 全量替换聚合后的 usage_records。
pub(super) fn replace_usage_records(conn: &Connection, records: &[UsageRecord]) -> Result<()> {
    conn.execute_batch("DELETE FROM usage_records")?;
    let mut stmt = conn.prepare(
        "INSERT INTO usage_records
         (agent, model, date, in_tokens, out_tokens,
          cache_read_input_tokens, cache_creation_input_tokens)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for r in records {
        stmt.execute(params![
            r.agent,
            r.model,
            r.date,
            r.in_tokens,
            r.out_tokens,
            r.cache_read_input_tokens,
            r.cache_creation_input_tokens,
        ])?;
    }
    Ok(())
}

/// 清理不再存在的源目录下的 stale 文件条目。
pub(super) fn cleanup_stale_entries(
    conn: &Connection,
    active_source_roots: &[&Path],
    active_extra_files: &[&Path],
) -> Result<()> {
    let paths: Vec<String> = {
        let mut stmt = conn.prepare("SELECT path FROM scanned_files")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(Result::ok).collect()
    };

    for path_str in &paths {
        let path = Path::new(path_str);
        let is_active = active_source_roots
            .iter()
            .any(|root| path.starts_with(root))
            || active_extra_files
                .iter()
                .any(|extra| path == *extra);
        if !is_active {
            conn.execute("DELETE FROM scanned_files WHERE path = ?1", params![path_str])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Connection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_db(&path).unwrap();
        init_schema(&conn).unwrap();
        (conn, dir)
    }

    #[test]
    fn schema_initialization_is_idempotent() {
        let (conn, _dir) = temp_db();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
    }

    #[test]
    fn store_and_load_raw_entries() {
        let (conn, _dir) = temp_db();
        let entries = vec![RawEntry {
            agent: "test".to_string(),
            model: "m".to_string(),
            date: "2026-01-01".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 5,
            reasoning_output_tokens: 0,
            dedup_primary: None,
            dedup_secondary: None,
            is_sidechain: false,
        }];
        store_file_entries(&conn, "/test/file.jsonl", 1000, 200, &entries).unwrap();

        assert!(file_unchanged(&conn, "/test/file.jsonl", 1000, 200));
        assert!(!file_unchanged(&conn, "/test/file.jsonl", 1001, 200));

        let loaded = get_cached_raw(&conn, "/test/file.jsonl").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].input_tokens, 100);
    }

    #[test]
    fn replace_and_load_usage_records() {
        let (conn, _dir) = temp_db();
        let records = vec![UsageRecord {
            agent: "claude".to_string(),
            model: "opus-4".to_string(),
            date: "2026-01-01".to_string(),
            in_tokens: 1000,
            total_tokens: 1500,
            out_tokens: 500,
            cache_read_input_tokens: 100,
            cache_creation_input_tokens: 50,
        }];
        replace_usage_records(&conn, &records).unwrap();

        let loaded = load_aggregated(&conn).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].in_tokens, 1000);
    }
}
