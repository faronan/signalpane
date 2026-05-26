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
    ListEvents { unread_only: bool, limit: usize },
    Sources,
    MarkRead { id: i64 },
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

    pub fn list_events(&self, unread_only: bool, limit: usize) -> Result<Vec<Event>> {
        match self.send(&IpcRequest::ListEvents { unread_only, limit })? {
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
        IpcRequest::ListEvents { unread_only, limit } => Ok(IpcResponse::Events {
            events: store.list_events(unread_only, limit)?,
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
}
