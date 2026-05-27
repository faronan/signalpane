use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Map, Value};

use crate::{
    collectors::EventDraft,
    model::{Account, AppStatus, Event, SourceStatus},
};

const MIGRATION_001: &str = include_str!("../../migrations/001_init.sql");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCursor {
    pub cursor_value: Option<String>,
    pub poll_after: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: u32,
}

#[derive(Debug, Clone)]
struct StoredCursor {
    cursor_value: Option<String>,
    poll_after: Option<DateTime<Utc>>,
    metadata_json: Value,
}

#[derive(Debug, Clone, Default)]
struct CursorHealth {
    last_success_at: Option<DateTime<Utc>>,
    last_error_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
    consecutive_failures: u32,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open SQLite database {}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(MIGRATION_001)?;
        Ok(())
    }

    pub fn upsert_account(
        &self,
        source: &str,
        label: &str,
        external_account_id: &str,
        enabled: bool,
        config_json: &serde_json::Value,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO accounts(source, label, external_account_id, enabled, config_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
            ON CONFLICT(source, external_account_id) DO UPDATE SET
                label = excluded.label,
                enabled = excluded.enabled,
                config_json = excluded.config_json,
                updated_at = excluded.updated_at
            "#,
            params![
                source,
                label,
                external_account_id,
                enabled as i64,
                config_json.to_string(),
                now
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM accounts WHERE source = ?1 AND external_account_id = ?2",
                params![source, external_account_id],
                |row| row.get(0),
            )
            .context("failed to fetch upserted account id")
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source, label, external_account_id, enabled FROM accounts ORDER BY source, label",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                source: row.get(1)?,
                label: row.get(2)?,
                external_account_id: row.get(3)?,
                enabled: row.get::<_, i64>(4)? != 0,
            })
        })?;
        collect_rows(rows)
    }

    pub fn upsert_event(&self, draft: &EventDraft) -> Result<i64> {
        let received_at = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO events(
                source, account_id, external_id, title, body, url, actor, reason,
                occurred_at, received_at, raw_json
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(source, account_id, external_id) DO UPDATE SET
                title = excluded.title,
                body = excluded.body,
                url = excluded.url,
                actor = excluded.actor,
                reason = excluded.reason,
                occurred_at = excluded.occurred_at,
                raw_json = excluded.raw_json
            "#,
            params![
                draft.source,
                draft.account_id,
                draft.external_id,
                draft.title,
                draft.body,
                draft.url,
                draft.actor,
                draft.reason,
                draft.occurred_at.to_rfc3339(),
                received_at,
                draft.raw_json.to_string(),
            ],
        )?;
        self.conn
            .query_row(
                "SELECT id FROM events WHERE source = ?1 AND account_id = ?2 AND external_id = ?3",
                params![draft.source, draft.account_id, draft.external_id],
                |row| row.get(0),
            )
            .context("failed to fetch upserted event id")
    }

    pub fn list_events(&self, unread_only: bool, limit: usize) -> Result<Vec<Event>> {
        let sql = if unread_only {
            r#"
            SELECT id, source, account_id, external_id, title, body, url, actor, reason,
                   occurred_at, received_at, read_at, raw_json
            FROM events
            WHERE read_at IS NULL
            ORDER BY occurred_at DESC
            LIMIT ?1
            "#
        } else {
            r#"
            SELECT id, source, account_id, external_id, title, body, url, actor, reason,
                   occurred_at, received_at, read_at, raw_json
            FROM events
            ORDER BY occurred_at DESC
            LIMIT ?1
            "#
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit as i64], event_from_row)?;
        collect_rows(rows)
    }

    pub fn mark_read(&self, id: i64) -> Result<bool> {
        let affected = self.conn.execute(
            "UPDATE events SET read_at = ?1 WHERE id = ?2 AND read_at IS NULL",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(affected > 0)
    }

    pub fn set_cursor(
        &self,
        source: &str,
        cursor_key: &str,
        cursor_value: Option<&str>,
        poll_after: Option<DateTime<Utc>>,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            r#"
            INSERT INTO source_cursors(source, cursor_key, cursor_value, poll_after, metadata_json, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(source, cursor_key) DO UPDATE SET
                cursor_value = excluded.cursor_value,
                poll_after = excluded.poll_after,
                metadata_json = excluded.metadata_json,
                updated_at = excluded.updated_at
            "#,
            params![
                source,
                cursor_key,
                cursor_value,
                poll_after.map(|dt| dt.to_rfc3339()),
                metadata.to_string(),
                now
            ],
        )?;
        Ok(())
    }

    pub fn record_cursor_success(
        &self,
        source: &str,
        cursor_key: &str,
        cursor_value: Option<&str>,
        poll_after: Option<DateTime<Utc>>,
        metadata: &Value,
    ) -> Result<()> {
        let previous = self.read_cursor(source, cursor_key)?;
        let mut metadata_json = merged_metadata(
            previous.as_ref().map(|cursor| &cursor.metadata_json),
            metadata,
        );
        metadata_json.insert(
            "last_success_at".to_string(),
            Value::String(Utc::now().to_rfc3339()),
        );
        metadata_json.remove("last_error_at");
        metadata_json.remove("last_error");
        metadata_json.insert(
            "consecutive_failures".to_string(),
            Value::Number(serde_json::Number::from(0)),
        );
        self.set_cursor(
            source,
            cursor_key,
            cursor_value,
            poll_after,
            &Value::Object(metadata_json),
        )
    }

    pub fn record_cursor_failure(
        &self,
        source: &str,
        cursor_key: &str,
        error: &str,
        consecutive_failures: u32,
        poll_after: Option<DateTime<Utc>>,
        metadata: &Value,
    ) -> Result<()> {
        let previous = self.read_cursor(source, cursor_key)?;
        let cursor_value = previous
            .as_ref()
            .and_then(|cursor| cursor.cursor_value.as_deref());
        let mut metadata_json = merged_metadata(
            previous.as_ref().map(|cursor| &cursor.metadata_json),
            metadata,
        );
        metadata_json.insert(
            "last_error_at".to_string(),
            Value::String(Utc::now().to_rfc3339()),
        );
        metadata_json.insert("last_error".to_string(), Value::String(error.to_string()));
        metadata_json.insert(
            "consecutive_failures".to_string(),
            Value::Number(serde_json::Number::from(consecutive_failures)),
        );
        self.set_cursor(
            source,
            cursor_key,
            cursor_value,
            poll_after,
            &Value::Object(metadata_json),
        )
    }

    pub fn get_cursor(&self, source: &str, cursor_key: &str) -> Result<Option<String>> {
        Ok(self
            .get_cursor_state(source, cursor_key)?
            .and_then(|cursor| cursor.cursor_value))
    }

    pub fn get_cursor_state(&self, source: &str, cursor_key: &str) -> Result<Option<SourceCursor>> {
        Ok(self
            .read_cursor(source, cursor_key)?
            .map(source_cursor_from_stored))
    }

    pub fn app_status(&self) -> Result<AppStatus> {
        let unread_count =
            self.scalar_count("SELECT COUNT(*) FROM events WHERE read_at IS NULL")?;
        let total_count = self.scalar_count("SELECT COUNT(*) FROM events")?;
        Ok(AppStatus {
            unread_count,
            total_count,
            sources: self.source_statuses()?,
        })
    }

    pub fn source_statuses(&self) -> Result<Vec<SourceStatus>> {
        let accounts = self.list_accounts()?;
        accounts
            .into_iter()
            .map(|account| {
                let unread_count = self.conn.query_row(
                    "SELECT COUNT(*) FROM events WHERE account_id = ?1 AND read_at IS NULL",
                    params![account.id],
                    |row| row.get(0),
                )?;
                let cursor = self
                    .conn
                    .query_row(
                        r#"
                    SELECT cursor_value, poll_after, metadata_json
                    FROM source_cursors
                    WHERE source = ?1
                    ORDER BY updated_at DESC
                    LIMIT 1
                    "#,
                        params![account.source],
                        stored_cursor_from_row,
                    )
                    .optional()?;
                let health = cursor
                    .as_ref()
                    .map(|cursor| cursor_health(&cursor.metadata_json))
                    .unwrap_or_default();
                Ok(SourceStatus {
                    source: account.source,
                    label: account.label,
                    enabled: account.enabled,
                    unread_count,
                    last_cursor: cursor
                        .as_ref()
                        .and_then(|cursor| cursor.cursor_value.clone()),
                    poll_after: cursor.and_then(|cursor| cursor.poll_after),
                    last_success_at: health.last_success_at,
                    last_error_at: health.last_error_at,
                    last_error: health.last_error,
                    consecutive_failures: health.consecutive_failures,
                })
            })
            .collect()
    }

    fn read_cursor(&self, source: &str, cursor_key: &str) -> Result<Option<StoredCursor>> {
        self.conn
            .query_row(
                "SELECT cursor_value, poll_after, metadata_json FROM source_cursors WHERE source = ?1 AND cursor_key = ?2",
                params![source, cursor_key],
                stored_cursor_from_row,
            )
            .optional()
            .context("failed to get source cursor")
    }

    fn scalar_count(&self, sql: &str) -> Result<i64> {
        self.conn
            .query_row(sql, [], |row| row.get(0))
            .context("failed to query count")
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        source: row.get(1)?,
        account_id: row.get(2)?,
        external_id: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        url: row.get(6)?,
        actor: row.get(7)?,
        reason: row.get(8)?,
        occurred_at: parse_dt(row.get::<_, String>(9)?)?,
        received_at: parse_dt(row.get::<_, String>(10)?)?,
        read_at: row
            .get::<_, Option<String>>(11)?
            .map(parse_dt)
            .transpose()?,
        raw_json: serde_json::from_str(&row.get::<_, String>(12)?).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Text,
                Box::new(err),
            )
        })?,
    })
}

fn stored_cursor_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCursor> {
    let poll_after = row
        .get::<_, Option<String>>(1)?
        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
        .map(|value| value.with_timezone(&Utc));
    let metadata_json = serde_json::from_str(&row.get::<_, String>(2)?)
        .unwrap_or_else(|_| Value::Object(Map::new()));
    Ok(StoredCursor {
        cursor_value: row.get(0)?,
        poll_after,
        metadata_json,
    })
}

fn source_cursor_from_stored(cursor: StoredCursor) -> SourceCursor {
    let health = cursor_health(&cursor.metadata_json);
    SourceCursor {
        cursor_value: cursor.cursor_value,
        poll_after: cursor.poll_after,
        last_success_at: health.last_success_at,
        last_error_at: health.last_error_at,
        last_error: health.last_error,
        consecutive_failures: health.consecutive_failures,
    }
}

fn cursor_health(metadata: &Value) -> CursorHealth {
    CursorHealth {
        last_success_at: metadata_datetime(metadata, "last_success_at"),
        last_error_at: metadata_datetime(metadata, "last_error_at"),
        last_error: metadata
            .get("last_error")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        consecutive_failures: metadata
            .get("consecutive_failures")
            .and_then(Value::as_u64)
            .and_then(|value| value.try_into().ok())
            .unwrap_or(0),
    }
}

fn metadata_datetime(metadata: &Value, key: &str) -> Option<DateTime<Utc>> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn merged_metadata(previous: Option<&Value>, overlay: &Value) -> Map<String, Value> {
    let mut metadata = previous
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(overlay) = overlay.as_object() {
        for (key, value) in overlay {
            metadata.insert(key.clone(), value.clone());
        }
    }
    metadata
}

fn parse_dt(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to collect SQLite rows")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn upserts_events_persists_cursor_and_marks_read() {
        let store = Store::open_in_memory().expect("store");
        let account_id = store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("account");
        let draft = EventDraft {
            source: "github".to_string(),
            account_id,
            external_id: "gh-1".to_string(),
            title: "title".to_string(),
            body: Some("body".to_string()),
            url: None,
            actor: None,
            reason: Some("mention".to_string()),
            occurred_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 2, 3).single().unwrap(),
            raw_json: serde_json::json!({"id": "gh-1"}),
        };

        let id = store.upsert_event(&draft).expect("event");
        store
            .set_cursor(
                "github",
                "notifications",
                Some("Tue, 26 May 2026 01:02:03 GMT"),
                Some(Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap()),
                &serde_json::json!({}),
            )
            .expect("cursor");

        assert_eq!(store.list_events(true, 10).expect("events").len(), 1);
        assert_eq!(
            store.get_cursor("github", "notifications").expect("cursor"),
            Some("Tue, 26 May 2026 01:02:03 GMT".to_string())
        );
        assert_eq!(
            store
                .get_cursor_state("github", "notifications")
                .expect("cursor")
                .and_then(|cursor| cursor.poll_after),
            Some(Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap())
        );
        assert!(store.mark_read(id).expect("mark read"));
        assert!(store.list_events(true, 10).expect("events").is_empty());
    }

    #[test]
    fn cursor_failure_metadata_surfaces_in_source_status() {
        let store = Store::open_in_memory().expect("store");
        store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("account");
        let poll_after = Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap();

        store
            .record_cursor_failure(
                "github",
                "notifications",
                "request timed out",
                2,
                Some(poll_after),
                &serde_json::json!({}),
            )
            .expect("failure metadata");

        let statuses = store.source_statuses().expect("statuses");
        let github = statuses
            .iter()
            .find(|status| status.source == "github")
            .expect("github status");

        assert_eq!(github.poll_after, Some(poll_after));
        assert_eq!(github.last_error.as_deref(), Some("request timed out"));
        assert_eq!(github.consecutive_failures, 2);
        assert!(github.last_error_at.is_some());
        assert_eq!(github.last_success_at, None);
    }

    #[test]
    fn cursor_success_clears_error_metadata() {
        let store = Store::open_in_memory().expect("store");
        store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("account");

        store
            .record_cursor_failure(
                "github",
                "notifications",
                "request timed out",
                3,
                None,
                &serde_json::json!({}),
            )
            .expect("failure metadata");
        store
            .record_cursor_success(
                "github",
                "notifications",
                Some("Tue, 26 May 2026 01:02:03 GMT"),
                None,
                &serde_json::json!({}),
            )
            .expect("success metadata");

        let statuses = store.source_statuses().expect("statuses");
        let github = statuses
            .iter()
            .find(|status| status.source == "github")
            .expect("github status");

        assert_eq!(
            github.last_cursor.as_deref(),
            Some("Tue, 26 May 2026 01:02:03 GMT")
        );
        assert_eq!(github.last_error, None);
        assert_eq!(github.last_error_at, None);
        assert_eq!(github.consecutive_failures, 0);
        assert!(github.last_success_at.is_some());
    }
}
