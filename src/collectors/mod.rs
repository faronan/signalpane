use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod github;
pub mod slack;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventDraft {
    pub source: String,
    pub account_id: i64,
    pub external_id: String,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<String>,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub raw_json: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectOutcome {
    pub events: Vec<EventDraft>,
    pub cursor_value: Option<String>,
    pub poll_after: Option<DateTime<Utc>>,
}

impl CollectOutcome {
    pub fn empty() -> Self {
        Self {
            events: Vec::new(),
            cursor_value: None,
            poll_after: None,
        }
    }
}
