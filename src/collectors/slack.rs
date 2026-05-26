use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeZone, Utc};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::Deserialize;

use super::{CollectOutcome, EventDraft};

const SOURCE: &str = "slack";
const AUTH_TEST_URL: &str = "https://slack.com/api/auth.test";
const HISTORY_URL: &str = "https://slack.com/api/conversations.history";

#[derive(Debug, Clone)]
pub struct SlackCollector {
    client: Client,
    token: String,
    user_id: Option<String>,
}

impl SlackCollector {
    pub fn new(token: String, user_id: Option<String>) -> Self {
        Self {
            client: Client::new(),
            token,
            user_id,
        }
    }

    pub fn collect_channel(
        &self,
        account_id: i64,
        channel_id: &str,
        oldest: Option<&str>,
    ) -> Result<CollectOutcome> {
        let user_id = match &self.user_id {
            Some(user_id) => user_id.clone(),
            None => self.resolve_user_id()?,
        };
        let mut request = self
            .client
            .get(HISTORY_URL)
            .headers(auth_headers(&self.token)?)
            .query(&[("channel", channel_id), ("limit", "100")]);
        if let Some(oldest) = oldest {
            request = request.query(&[("oldest", oldest), ("inclusive", "false")]);
        }

        let response = request.send()?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            let poll_after = parse_retry_after(response.headers());
            return Ok(CollectOutcome {
                events: Vec::new(),
                cursor_value: oldest.map(ToOwned::to_owned),
                poll_after,
            });
        }
        if !response.status().is_success() {
            bail!(
                "Slack conversations.history failed with status {}",
                response.status()
            );
        }
        let history: SlackHistoryResponse = response.json()?;
        if !history.ok {
            bail!(
                "Slack conversations.history returned error: {}",
                history.error.unwrap_or_else(|| "unknown".to_string())
            );
        }
        let latest_ts = latest_message_ts(&history.messages);
        let events =
            slack_messages_to_event_drafts(account_id, channel_id, &user_id, history.messages)?;
        let cursor_value = latest_ts.or_else(|| oldest.map(ToOwned::to_owned));
        Ok(CollectOutcome {
            events,
            cursor_value,
            poll_after: None,
        })
    }

    fn resolve_user_id(&self) -> Result<String> {
        let response = self
            .client
            .get(AUTH_TEST_URL)
            .headers(auth_headers(&self.token)?)
            .send()?;
        if !response.status().is_success() {
            bail!("Slack auth.test failed with status {}", response.status());
        }
        let auth: SlackAuthResponse = response.json()?;
        if !auth.ok {
            bail!(
                "Slack auth.test returned error: {}",
                auth.error.unwrap_or_else(|| "unknown".to_string())
            );
        }
        auth.user_id
            .context("Slack auth.test did not return user_id")
    }
}

fn auth_headers(token: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))
            .context("failed to build Slack authorization header")?,
    );
    Ok(headers)
}

pub fn slack_messages_to_event_drafts(
    account_id: i64,
    channel_id: &str,
    user_id: &str,
    messages: Vec<SlackMessage>,
) -> Result<Vec<EventDraft>> {
    let needle = format!("<@{user_id}>");
    messages
        .into_iter()
        .filter(|message| {
            message
                .text
                .as_deref()
                .is_some_and(|text| text.contains(&needle))
        })
        .map(|message| message.into_event_draft(account_id, channel_id))
        .collect()
}

pub fn latest_message_ts(messages: &[SlackMessage]) -> Option<String> {
    messages
        .iter()
        .map(|message| message.ts.as_str())
        .max()
        .map(ToOwned::to_owned)
}

pub fn parse_retry_after(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let seconds = headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())?;
    Some(Utc::now() + chrono::Duration::seconds(seconds))
}

#[derive(Debug, Clone, Deserialize)]
pub struct SlackHistoryResponse {
    pub ok: bool,
    #[serde(default)]
    pub messages: Vec<SlackMessage>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct SlackMessage {
    pub ts: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub permalink: Option<String>,
}

impl SlackMessage {
    fn into_event_draft(self, account_id: i64, channel_id: &str) -> Result<EventDraft> {
        let occurred_at = slack_ts_to_datetime(&self.ts)
            .with_context(|| format!("invalid Slack ts {}", self.ts))?;
        let raw_json = serde_json::to_value(&self)?;
        Ok(EventDraft {
            source: SOURCE.to_string(),
            account_id,
            external_id: format!("{channel_id}:{}", self.ts),
            title: format!("Slack mention in {channel_id}"),
            body: self.text,
            url: self.permalink,
            actor: self.user.or(self.username),
            reason: Some("mention".to_string()),
            occurred_at,
            raw_json,
        })
    }
}

fn slack_ts_to_datetime(ts: &str) -> Option<DateTime<Utc>> {
    let (seconds, fraction) = ts.split_once('.')?;
    let seconds = seconds.parse::<i64>().ok()?;
    let micros = fraction.get(0..6).unwrap_or(fraction).parse::<u32>().ok()?;
    Utc.timestamp_opt(seconds, micros * 1_000).single()
}

#[derive(Debug, Deserialize)]
struct SlackAuthResponse {
    ok: bool,
    user_id: Option<String>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_only_direct_user_mentions() {
        let fixture = include_str!("../../fixtures/slack/history.json");
        let history: SlackHistoryResponse = serde_json::from_str(fixture).expect("fixture parses");

        let events =
            slack_messages_to_event_drafts(9, "C123", "U123", history.messages).expect("events");

        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.source == "slack"));
        assert!(
            events
                .iter()
                .all(|event| event.body.as_deref().unwrap_or("").contains("<@U123>"))
        );
    }

    #[test]
    fn cursor_uses_latest_seen_message_not_latest_mention() {
        let fixture = include_str!("../../fixtures/slack/history.json");
        let history: SlackHistoryResponse = serde_json::from_str(fixture).expect("fixture parses");

        assert_eq!(
            latest_message_ts(&history.messages),
            Some("1779757327.000100".to_string())
        );
    }
}
