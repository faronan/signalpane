use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    model::{AppStatus, Event, SourceStatus},
    store::Store,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    Status,
    ListEvents {
        unread_only: bool,
        source: Option<String>,
        limit: usize,
    },
    Sources,
    MarkRead {
        id: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Ok,
    Error { message: String },
    Status { status: AppStatus },
    Events { events: Vec<Event> },
    Sources { sources: Vec<SourceStatus> },
    MarkRead { changed: bool },
}

pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub fn status(&self) -> Result<AppStatus> {
        match self.send(&IpcRequest::Status)? {
            IpcResponse::Status { status } => Ok(status),
            other => bail!("unexpected IPC response: {other:?}"),
        }
    }

    pub fn list_events(
        &self,
        unread_only: bool,
        source: Option<String>,
        limit: usize,
    ) -> Result<Vec<Event>> {
        match self.send(&IpcRequest::ListEvents {
            unread_only,
            source,
            limit,
        })? {
            IpcResponse::Events { events } => Ok(events),
            other => bail!("unexpected IPC response: {other:?}"),
        }
    }

    pub fn sources(&self) -> Result<Vec<SourceStatus>> {
        match self.send(&IpcRequest::Sources)? {
            IpcResponse::Sources { sources } => Ok(sources),
            other => bail!("unexpected IPC response: {other:?}"),
        }
    }

    pub fn mark_read(&self, id: i64) -> Result<bool> {
        match self.send(&IpcRequest::MarkRead { id })? {
            IpcResponse::MarkRead { changed } => Ok(changed),
            other => bail!("unexpected IPC response: {other:?}"),
        }
    }

    pub fn send(&self, request: &IpcRequest) -> Result<IpcResponse> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("failed to connect {}", self.socket_path.display()))?;
        let payload = serde_json::to_string(request)?;
        stream.write_all(payload.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        reader.read_line(&mut line)?;
        let response: IpcResponse = serde_json::from_str(line.trim_end())?;
        match response {
            IpcResponse::Error { message } => bail!(message),
            response => Ok(response),
        }
    }
}

pub fn handle_request(db_path: &Path, request: IpcRequest) -> IpcResponse {
    match handle_request_result(db_path, request) {
        Ok(response) => response,
        Err(err) => IpcResponse::Error {
            message: err.to_string(),
        },
    }
}

fn handle_request_result(db_path: &Path, request: IpcRequest) -> Result<IpcResponse> {
    let store = Store::open(db_path)?;
    match request {
        IpcRequest::Status => Ok(IpcResponse::Status {
            status: store.app_status()?,
        }),
        IpcRequest::ListEvents {
            unread_only,
            source,
            limit,
        } => Ok(IpcResponse::Events {
            events: store.list_events(unread_only, source.as_deref(), limit)?,
        }),
        IpcRequest::Sources => Ok(IpcResponse::Sources {
            sources: store.source_statuses()?,
        }),
        IpcRequest::MarkRead { id } => Ok(IpcResponse::MarkRead {
            changed: store.mark_read(id)?,
        }),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    use crate::collectors::EventDraft;

    use super::*;

    #[test]
    fn handles_status_and_mark_read_requests() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("signalpane.sqlite3");
        let store = Store::open(&db_path).expect("store");
        let account_id = store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("account");
        let event_id = store
            .upsert_event(&EventDraft {
                source: "github".to_string(),
                account_id,
                external_id: "gh-1".to_string(),
                title: "Mention".to_string(),
                body: None,
                url: None,
                actor: None,
                reason: Some("mention".to_string()),
                occurred_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 2, 3).single().unwrap(),
                raw_json: serde_json::json!({"id": "gh-1"}),
            })
            .expect("event");

        let status = handle_request(&db_path, IpcRequest::Status);
        assert!(matches!(
            status,
            IpcResponse::Status {
                status: AppStatus {
                    unread_count: 1,
                    ..
                }
            }
        ));

        let changed = handle_request(&db_path, IpcRequest::MarkRead { id: event_id });
        assert_eq!(changed, IpcResponse::MarkRead { changed: true });
    }

    #[test]
    fn exposes_source_health_in_status_and_sources() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("signalpane.sqlite3");
        let store = Store::open(&db_path).expect("store");
        store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("account");
        store
            .record_cursor_failure(
                "github",
                "notifications",
                "GitHub collector timed out",
                1,
                Some(Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap()),
                &serde_json::json!({}),
            )
            .expect("failure metadata");

        let status = handle_request(&db_path, IpcRequest::Status);
        let IpcResponse::Status { status } = status else {
            panic!("unexpected status response");
        };
        assert_eq!(
            status.sources[0].cursor_key.as_deref(),
            Some("notifications")
        );
        assert_eq!(
            status.sources[0].last_error.as_deref(),
            Some("GitHub collector timed out")
        );
        assert_eq!(status.sources[0].consecutive_failures, 1);

        let sources = handle_request(&db_path, IpcRequest::Sources);
        let IpcResponse::Sources { sources } = sources else {
            panic!("unexpected sources response");
        };
        assert_eq!(
            sources[0].last_error.as_deref(),
            Some("GitHub collector timed out")
        );
        assert_eq!(sources[0].consecutive_failures, 1);
    }

    #[test]
    fn list_events_filters_by_source_and_read_state() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("signalpane.sqlite3");
        let store = Store::open(&db_path).expect("store");
        let github_account = store
            .upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))
            .expect("github account");
        let slack_account = store
            .upsert_account("slack", "Slack", "default", true, &serde_json::json!({}))
            .expect("slack account");
        let github_id = store
            .upsert_event(&EventDraft {
                source: "github".to_string(),
                account_id: github_account,
                external_id: "gh-1".to_string(),
                title: "GitHub mention".to_string(),
                body: None,
                url: None,
                actor: Some("octocat".to_string()),
                reason: Some("mention".to_string()),
                occurred_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 2, 3).single().unwrap(),
                raw_json: serde_json::json!({"id": "gh-1"}),
            })
            .expect("github event");
        store
            .upsert_event(&EventDraft {
                source: "slack".to_string(),
                account_id: slack_account,
                external_id: "slack-1".to_string(),
                title: "Slack mention".to_string(),
                body: None,
                url: None,
                actor: Some("U123".to_string()),
                reason: Some("mention".to_string()),
                occurred_at: Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap(),
                raw_json: serde_json::json!({"ts": "1779757383.000100"}),
            })
            .expect("slack event");
        assert!(store.mark_read(github_id).expect("mark github read"));

        let unread = handle_request(
            &db_path,
            IpcRequest::ListEvents {
                unread_only: true,
                source: None,
                limit: 10,
            },
        );
        let IpcResponse::Events { events } = unread else {
            panic!("unexpected unread response");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, "slack");

        let github = handle_request(
            &db_path,
            IpcRequest::ListEvents {
                unread_only: false,
                source: Some("github".to_string()),
                limit: 10,
            },
        );
        let IpcResponse::Events { events } = github else {
            panic!("unexpected github response");
        };
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].source, "github");
        assert!(events[0].read_at.is_some());
    }

    #[test]
    fn exposes_each_cursor_health_in_sources() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("signalpane.sqlite3");
        let store = Store::open(&db_path).expect("store");
        store
            .upsert_account(
                "slack",
                "Slack",
                "default",
                true,
                &serde_json::json!({ "channels": ["C1", "C2"] }),
            )
            .expect("account");
        store
            .record_cursor_failure(
                "slack",
                "channel:C1",
                "Slack collector timed out",
                1,
                Some(Utc.with_ymd_and_hms(2026, 5, 26, 1, 3, 3).single().unwrap()),
                &serde_json::json!({ "channel": "C1" }),
            )
            .expect("failure metadata");
        store
            .record_cursor_success(
                "slack",
                "channel:C2",
                Some("1779757327.000100"),
                Some(Utc.with_ymd_and_hms(2026, 5, 26, 1, 4, 3).single().unwrap()),
                &serde_json::json!({ "channel": "C2" }),
            )
            .expect("success metadata");

        let sources = handle_request(&db_path, IpcRequest::Sources);
        let IpcResponse::Sources { sources } = sources else {
            panic!("unexpected sources response");
        };

        assert_eq!(sources.len(), 2);
        assert!(
            sources
                .iter()
                .any(|source| source.cursor_key.as_deref() == Some("channel:C1")
                    && source.last_error.as_deref() == Some("Slack collector timed out"))
        );
        assert!(
            sources
                .iter()
                .any(|source| source.cursor_key.as_deref() == Some("channel:C2")
                    && source.last_cursor.as_deref() == Some("1779757327.000100"))
        );
    }
}
