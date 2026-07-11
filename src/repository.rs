use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::domain::{LogSample, SampleKind};

const SCHEMA_VERSION: i64 = 2;

pub struct Repository {
    connection: Connection,
}

#[derive(Debug, Clone)]
pub struct OutcomeRow {
    pub model_name: String,
    pub group_name: String,
    pub outcome: String,
    pub last_seen: i64,
    pub total_ms: Option<i64>,
    pub ttft_ms: Option<i64>,
    pub attempt_count: i64,
    pub error_count: i64,
    pub latest_error_code: String,
    pub latest_retry_channel_chain: String,
}

#[derive(Debug, Clone, Default)]
pub struct DatabaseStats {
    pub sample_count: i64,
    pub outcome_count: i64,
}

impl Repository {
    /// 打开插件私有 SQLite，并完成幂等迁移。
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("创建数据库目录失败: {error}"))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("打开 SQLite 失败 {}: {error}", path.display()))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("设置 SQLite busy timeout 失败: {error}"))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS log_samples (
                    sample_key TEXT PRIMARY KEY,
                    source_log_id INTEGER NOT NULL,
                    request_id TEXT NOT NULL,
                    upstream_request_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    log_type INTEGER NOT NULL,
                    model_name TEXT NOT NULL,
                    group_name TEXT NOT NULL,
                    channel_id INTEGER NOT NULL,
                    channel_name TEXT NOT NULL,
                    is_stream INTEGER NOT NULL,
                    prompt_tokens INTEGER NOT NULL,
                    completion_tokens INTEGER NOT NULL,
                    total_ms INTEGER,
                    ttft_ms INTEGER,
                    partial_failure INTEGER NOT NULL,
                    attempt_index INTEGER NOT NULL,
                    retry_channel_chain TEXT NOT NULL DEFAULT '',
                    error_code TEXT NOT NULL,
                    status_code INTEGER,
                    error_message TEXT NOT NULL,
                    ingested_at INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_samples_model_time
                    ON log_samples(model_name, created_at);
                 CREATE INDEX IF NOT EXISTS idx_samples_request
                    ON log_samples(request_id, model_name);
                 CREATE TABLE IF NOT EXISTS request_outcomes (
                    request_id TEXT NOT NULL,
                    model_name TEXT NOT NULL,
                    group_name TEXT NOT NULL,
                    first_seen INTEGER NOT NULL,
                    last_seen INTEGER NOT NULL,
                    outcome TEXT NOT NULL,
                    has_consume INTEGER NOT NULL,
                    total_ms INTEGER,
                    ttft_ms INTEGER,
                    attempt_count INTEGER NOT NULL,
                    error_count INTEGER NOT NULL,
                    latest_error_code TEXT NOT NULL,
                    latest_error_message TEXT NOT NULL,
                    latest_retry_channel_chain TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY(request_id, model_name)
                 );
                 CREATE INDEX IF NOT EXISTS idx_outcomes_model_time
                    ON request_outcomes(model_name, last_seen);
                 CREATE TABLE IF NOT EXISTS collector_state (
                    model_name TEXT PRIMARY KEY,
                    cursor_created_at INTEGER NOT NULL DEFAULT 0,
                    last_success_at INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT NOT NULL DEFAULT '',
                    consecutive_failures INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS perf_metric_cache (
                    model_name TEXT NOT NULL,
                    hours INTEGER NOT NULL,
                    fetched_at INTEGER NOT NULL,
                    response_json TEXT NOT NULL,
                    PRIMARY KEY(model_name, hours)
                 );",
            )
            .map_err(|error| format!("迁移 SQLite 失败: {error}"))?;
        migrate_schema(&connection)?;
        Ok(Self { connection })
    }

    /// 插入样本并在同一事务中更新请求级结果。返回 false 表示重复样本。
    pub fn ingest(&mut self, sample: &LogSample, ingested_at: i64) -> Result<bool, String> {
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| format!("开启 SQLite 事务失败: {error}"))?;
        let inserted = transaction
            .execute(
                "INSERT OR IGNORE INTO log_samples (
                    sample_key, source_log_id, request_id, upstream_request_id, created_at,
                    log_type, model_name, group_name, channel_id, channel_name, is_stream,
                    prompt_tokens, completion_tokens, total_ms, ttft_ms, partial_failure,
                    attempt_index, retry_channel_chain, error_code, status_code, error_message,
                    ingested_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
                params![
                    sample.sample_key,
                    sample.source_log_id,
                    sample.request_id,
                    sample.upstream_request_id,
                    sample.created_at,
                    sample.kind.as_i64(),
                    sample.model_name,
                    sample.group_name,
                    sample.channel_id,
                    sample.channel_name,
                    sample.is_stream as i64,
                    sample.prompt_tokens,
                    sample.completion_tokens,
                    sample.total_ms,
                    sample.ttft_ms,
                    sample.partial_failure as i64,
                    sample.attempt_index,
                    sample.retry_channel_chain,
                    sample.error_code,
                    sample.status_code,
                    sample.error_message,
                    ingested_at,
                ],
            )
            .map_err(|error| format!("写入日志样本失败: {error}"))?
            > 0;
        if inserted {
            update_outcome(&transaction, sample)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("提交 SQLite 事务失败: {error}"))?;
        Ok(inserted)
    }

    pub fn settle_pending(&self, cutoff: i64) -> Result<usize, String> {
        self.connection
            .execute(
                "UPDATE request_outcomes SET outcome='failed'
                 WHERE outcome='pending' AND has_consume=0 AND last_seen <= ?1",
                [cutoff],
            )
            .map_err(|error| format!("结算失败请求失败: {error}"))
    }

    pub fn cursor(&self, model: &str) -> Result<i64, String> {
        self.connection
            .query_row(
                "SELECT cursor_created_at FROM collector_state WHERE model_name=?1",
                [model],
                |row| row.get(0),
            )
            .optional()
            .map(|value| value.unwrap_or(0))
            .map_err(|error| format!("读取采集游标失败: {error}"))
    }

    pub fn record_success(&self, model: &str, cursor: i64, now: i64) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO collector_state(model_name,cursor_created_at,last_success_at,last_error,consecutive_failures)
                 VALUES(?1,?2,?3,'',0)
                 ON CONFLICT(model_name) DO UPDATE SET
                    cursor_created_at=excluded.cursor_created_at,
                    last_success_at=excluded.last_success_at,
                    last_error='',consecutive_failures=0",
                params![model, cursor, now],
            )
            .map(|_| ())
            .map_err(|error| format!("更新采集游标失败: {error}"))
    }

    pub fn record_failure(&self, model: &str, error: &str) -> Result<(), String> {
        self.connection
            .execute(
                "INSERT INTO collector_state(model_name,last_error,consecutive_failures)
                 VALUES(?1,?2,1)
                 ON CONFLICT(model_name) DO UPDATE SET
                    last_error=excluded.last_error,
                    consecutive_failures=collector_state.consecutive_failures+1",
                params![model, error],
            )
            .map(|_| ())
            .map_err(|db_error| format!("记录采集失败状态失败: {db_error}"))
    }

    pub fn outcomes_since(&self, cutoff: i64) -> Result<Vec<OutcomeRow>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT model_name,group_name,outcome,last_seen,total_ms,ttft_ms,
                        attempt_count,error_count,latest_error_code,latest_retry_channel_chain
                 FROM request_outcomes
                 WHERE last_seen >= ?1 AND outcome <> 'pending'",
            )
            .map_err(|error| format!("准备统计查询失败: {error}"))?;
        let rows = statement
            .query_map([cutoff], |row| {
                Ok(OutcomeRow {
                    model_name: row.get(0)?,
                    group_name: row.get(1)?,
                    outcome: row.get(2)?,
                    last_seen: row.get(3)?,
                    total_ms: row.get(4)?,
                    ttft_ms: row.get(5)?,
                    attempt_count: row.get(6)?,
                    error_count: row.get(7)?,
                    latest_error_code: row.get(8)?,
                    latest_retry_channel_chain: row.get(9)?,
                })
            })
            .map_err(|error| format!("执行统计查询失败: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("读取统计结果失败: {error}"))
    }

    pub fn cleanup(&self, cutoff: i64) -> Result<(), String> {
        self.connection
            .execute("DELETE FROM log_samples WHERE created_at < ?1", [cutoff])
            .map_err(|error| format!("清理过期日志失败: {error}"))?;
        self.connection
            .execute(
                "DELETE FROM request_outcomes WHERE last_seen < ?1",
                [cutoff],
            )
            .map_err(|error| format!("清理过期请求失败: {error}"))?;
        self.connection
            .execute(
                "DELETE FROM perf_metric_cache WHERE fetched_at < ?1",
                [cutoff],
            )
            .map_err(|error| format!("清理广场缓存失败: {error}"))?;
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE);")
            .map_err(|error| format!("执行 WAL checkpoint 失败: {error}"))?;
        Ok(())
    }

    pub fn stats(&self) -> Result<DatabaseStats, String> {
        let sample_count = self
            .connection
            .query_row("SELECT COUNT(*) FROM log_samples", [], |row| row.get(0))
            .map_err(|error| format!("统计日志行数失败: {error}"))?;
        let outcome_count = self
            .connection
            .query_row("SELECT COUNT(*) FROM request_outcomes", [], |row| {
                row.get(0)
            })
            .map_err(|error| format!("统计请求行数失败: {error}"))?;
        Ok(DatabaseStats {
            sample_count,
            outcome_count,
        })
    }
}

/// 将历史数据库幂等升级到当前版本；中断后再次打开可安全继续。
fn migrate_schema(connection: &Connection) -> Result<(), String> {
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|error| format!("读取 SQLite schema 版本失败: {error}"))?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "SQLite schema 版本 {version} 高于插件支持版本 {SCHEMA_VERSION}"
        ));
    }
    if !has_column(connection, "log_samples", "retry_channel_chain")? {
        connection
            .execute(
                "ALTER TABLE log_samples ADD COLUMN retry_channel_chain TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|error| format!("迁移日志重试链字段失败: {error}"))?;
    }
    if !has_column(connection, "request_outcomes", "latest_retry_channel_chain")? {
        connection
            .execute(
                "ALTER TABLE request_outcomes ADD COLUMN latest_retry_channel_chain TEXT NOT NULL DEFAULT ''",
                [],
            )
            .map_err(|error| format!("迁移请求重试链字段失败: {error}"))?;
    }
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|error| format!("更新 SQLite schema 版本失败: {error}"))?;
    Ok(())
}

fn has_column(connection: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("读取表结构失败 {table}: {error}"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("查询表结构失败 {table}: {error}"))?;
    for name in columns {
        if name.map_err(|error| format!("解析表结构失败 {table}: {error}"))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn update_outcome(transaction: &Transaction<'_>, sample: &LogSample) -> Result<(), String> {
    match sample.kind {
        SampleKind::Error => transaction.execute(
            "INSERT INTO request_outcomes(
                request_id,model_name,group_name,first_seen,last_seen,outcome,has_consume,
                total_ms,ttft_ms,attempt_count,error_count,latest_error_code,latest_error_message,
                latest_retry_channel_chain
             ) VALUES(?1,?2,?3,?4,?4,'pending',0,?5,NULL,1,1,?6,?7,?8)
             ON CONFLICT(request_id,model_name) DO UPDATE SET
                last_seen=MAX(request_outcomes.last_seen,excluded.last_seen),
                group_name=CASE WHEN request_outcomes.has_consume=0 THEN excluded.group_name ELSE request_outcomes.group_name END,
                outcome=CASE WHEN request_outcomes.has_consume=0 THEN 'pending' ELSE request_outcomes.outcome END,
                total_ms=CASE WHEN request_outcomes.has_consume=0 THEN excluded.total_ms ELSE request_outcomes.total_ms END,
                attempt_count=request_outcomes.attempt_count+1,
                error_count=request_outcomes.error_count+1,
                latest_error_code=excluded.latest_error_code,
                latest_error_message=excluded.latest_error_message,
                latest_retry_channel_chain=CASE
                    WHEN excluded.latest_retry_channel_chain<>'' THEN excluded.latest_retry_channel_chain
                    ELSE request_outcomes.latest_retry_channel_chain END",
            params![sample.request_id,sample.model_name,sample.group_name,sample.created_at,
                sample.total_ms,sample.error_code,sample.error_message,sample.retry_channel_chain],
        ),
        SampleKind::Consume => {
            let outcome = if sample.partial_failure { "partial_failed" } else { "success" };
            transaction.execute(
                "INSERT INTO request_outcomes(
                    request_id,model_name,group_name,first_seen,last_seen,outcome,has_consume,
                    total_ms,ttft_ms,attempt_count,error_count,latest_error_code,
                    latest_error_message,latest_retry_channel_chain
                 ) VALUES(?1,?2,?3,?4,?4,?5,1,?6,?7,1,0,'','',?8)
                 ON CONFLICT(request_id,model_name) DO UPDATE SET
                    last_seen=MAX(request_outcomes.last_seen,excluded.last_seen),
                    group_name=excluded.group_name,
                    outcome=excluded.outcome,
                    has_consume=1,
                    total_ms=excluded.total_ms,
                    ttft_ms=excluded.ttft_ms,
                    attempt_count=request_outcomes.error_count+1,
                    latest_retry_channel_chain=CASE
                        WHEN excluded.latest_retry_channel_chain<>'' THEN excluded.latest_retry_channel_chain
                        ELSE request_outcomes.latest_retry_channel_chain END",
                params![sample.request_id,sample.model_name,sample.group_name,sample.created_at,
                    outcome,sample.total_ms,sample.ttft_ms,sample.retry_channel_chain],
            )
        }
    }
    .map(|_| ())
    .map_err(|error| format!("归并请求结果失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SampleKind;
    use tempfile::tempdir;

    fn sample(key: &str, kind: SampleKind, partial: bool) -> LogSample {
        LogSample {
            sample_key: key.to_string(),
            source_log_id: 1,
            request_id: "req-1".to_string(),
            upstream_request_id: String::new(),
            created_at: 100,
            kind,
            model_name: "echo".to_string(),
            group_name: "default".to_string(),
            channel_id: 1,
            channel_name: "primary".to_string(),
            is_stream: true,
            prompt_tokens: 1,
            completion_tokens: 2,
            total_ms: Some(3000),
            ttft_ms: Some(500),
            partial_failure: partial,
            attempt_index: 1,
            retry_channel_chain: "1->2".to_string(),
            error_code: "upstream_error".to_string(),
            status_code: Some(500),
            error_message: "failed".to_string(),
        }
    }

    #[test]
    fn deduplicates_and_promotes_retry_to_success() {
        let dir = tempdir().unwrap();
        let mut repository = Repository::open(&dir.path().join("status.db")).unwrap();
        assert!(
            repository
                .ingest(&sample("error", SampleKind::Error, false), 101)
                .unwrap()
        );
        assert!(
            !repository
                .ingest(&sample("error", SampleKind::Error, false), 102)
                .unwrap()
        );
        assert!(
            repository
                .ingest(&sample("success", SampleKind::Consume, false), 103)
                .unwrap()
        );
        let rows = repository.outcomes_since(0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, "success");
        assert_eq!(rows[0].attempt_count, 2);
        assert_eq!(rows[0].error_count, 1);
        assert_eq!(rows[0].latest_retry_channel_chain, "1->2");
    }

    #[test]
    fn settles_terminal_failure_after_grace() {
        let dir = tempdir().unwrap();
        let mut repository = Repository::open(&dir.path().join("status.db")).unwrap();
        repository
            .ingest(&sample("error", SampleKind::Error, false), 101)
            .unwrap();
        repository.settle_pending(100).unwrap();
        assert_eq!(repository.outcomes_since(0).unwrap()[0].outcome, "failed");
    }

    #[test]
    fn migrates_existing_v1_database_to_retry_chain_schema() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.db");
        let repository = Repository::open(&path).unwrap();
        repository
            .connection
            .execute_batch(
                "ALTER TABLE log_samples DROP COLUMN retry_channel_chain;
                 ALTER TABLE request_outcomes DROP COLUMN latest_retry_channel_chain;
                 PRAGMA user_version=1;",
            )
            .unwrap();
        drop(repository);

        let migrated = Repository::open(&path).unwrap();
        assert!(has_column(&migrated.connection, "log_samples", "retry_channel_chain").unwrap());
        assert!(
            has_column(
                &migrated.connection,
                "request_outcomes",
                "latest_retry_channel_chain"
            )
            .unwrap()
        );
        let version = migrated
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
