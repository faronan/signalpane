use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    collectors::EventDraft,
    model::{Account, AppStatus, Event, SourceStatus},
};

const MIGRATION_001: &str = include_str!("../../migrations/001_init.sql");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceCursor {
    pub cursor_value: Option<String>,
    pub poll_after: Option<DateTime<Utc>>,
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

    pub fn get_cursor(&self, source: &str, cursor_key: &str) -> Result<Option<String>> {
        Ok(self
            .get_cursor_state(source, cursor_key)?
            .and_then(|cursor| cursor.cursor_value))
    }

    pub fn get_cursor_state(&self, source: &str, cursor_key: &str) -> Result<Option<SourceCursor>> {
        self.conn
            .query_row(
                "SELECT cursor_value, poll_after FROM source_cursors WHERE source = ?1 AND cursor_key = ?2",
                params![source, cursor_key],
                |row| {
                    let poll_after = row
                        .get::<_, Option<String>>(1)?
                        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                        .map(|value| value.with_timezone(&Utc));
                    Ok(SourceCursor {
                        cursor_value: row.get(0)?,
                        poll_after,
                    })
                },
            )
            .optional()
            .context("failed to get source cursor")
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
                    SELECT cursor_value, poll_after
                    FROM source_cursors
                    WHERE source = ?1
                    ORDER BY updated_at DESC
                    LIMIT 1
                    "#,
                        params![account.source],
                        |row| {
                            Ok((
                                row.get::<_, Option<String>>(0)?,
                                row.get::<_, Option<String>>(1)?,
                            ))
                        },
                    )
                    .optional()?;
                Ok(SourceStatus {
                    source: account.source,
                    label: account.label,
                    enabled: account.enabled,
                    unread_count,
                    last_cursor: cursor.as_ref().and_then(|(value, _)| value.clone()),
                    poll_after: cursor
                        .and_then(|(_, poll_after)| poll_after)
                        .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                        .map(|value| value.with_timezone(&Utc)),
                })
            })
            .collect()
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
}
