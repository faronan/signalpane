use std::time::Duration as StdDuration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use reqwest::{
    StatusCode,
    blocking::Client,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue, IF_MODIFIED_SINCE, USER_AGENT},
};
use serde::Deserialize;

use super::{CollectOutcome, EventDraft};

const SOURCE: &str = "github";
const NOTIFICATIONS_URL: &str =
    "https://api.github.com/notifications?all=false&participating=false";
const ACCEPT_VALUE: &str = "application/vnd.github+json";
const USER_AGENT_VALUE: &str = "signalpane/0.1";
const REASONS: &[&str] = &["mention", "team_mention", "review_requested"];
const API_TIMEOUT: StdDuration = StdDuration::from_secs(10);

#[derive(Debug, Clone)]
pub struct GithubCollector {
    client: Client,
    token: String,
}

impl GithubCollector {
    pub fn new(token: String) -> Self {
        Self {
            client: build_http_client(API_TIMEOUT),
            token,
        }
    }

    pub fn collect(&self, account_id: i64, last_modified: Option<&str>) -> Result<CollectOutcome> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_VALUE));
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.token))
                .context("failed to build GitHub authorization header")?,
        );
        if let Some(last_modified) = last_modified {
            headers.insert(
                IF_MODIFIED_SINCE,
                HeaderValue::from_str(last_modified)
                    .context("failed to build GitHub If-Modified-Since header")?,
            );
        }

        let response = self.client.get(NOTIFICATIONS_URL).headers(headers).send()?;
        let status = response.status();
        let last_modified_header = response
            .headers()
            .get("last-modified")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let poll_after = github_poll_after(response.headers());

        if status == StatusCode::NOT_MODIFIED {
            return Ok(CollectOutcome {
                events: Vec::new(),
                cursor_value: last_modified_header,
                poll_after,
            });
        }
        if !status.is_success() {
            bail!("GitHub notifications request failed with status {status}");
        }

        let threads: Vec<GithubThread> = response.json()?;
        Ok(CollectOutcome {
            events: github_threads_to_event_drafts(account_id, threads)?,
            cursor_value: last_modified_header,
            poll_after,
        })
    }
}

fn build_http_client(timeout: StdDuration) -> Client {
    Client::builder()
        .timeout(timeout)
        .build()
        .expect("failed to build GitHub HTTP client")
}

fn github_poll_after(headers: &HeaderMap) -> Option<DateTime<Utc>> {
    let interval = headers
        .get("x-poll-interval")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())?;
    Some(Utc::now() + Duration::seconds(interval))
}

pub fn github_threads_to_event_drafts(
    account_id: i64,
    threads: Vec<GithubThread>,
) -> Result<Vec<EventDraft>> {
    threads
        .into_iter()
        .filter(|thread| thread.unread)
        .filter(|thread| REASONS.contains(&thread.reason.as_str()))
        .map(|thread| thread.into_event_draft(account_id))
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubThread {
    pub id: String,
    pub unread: bool,
    pub reason: String,
    pub updated_at: DateTime<Utc>,
    pub subject: GithubSubject,
    pub repository: GithubRepository,
}

impl GithubThread {
    fn into_event_draft(self, account_id: i64) -> Result<EventDraft> {
        let title = format!("{}: {}", self.repository.full_name, self.subject.title);
        let raw_json = serde_json::to_value(&self)?;
        Ok(EventDraft {
            source: SOURCE.to_string(),
            account_id,
            external_id: self.id,
            title,
            body: Some(self.subject.kind),
            url: self
                .subject
                .latest_comment_url
                .or(self.subject.url)
                .or(self.repository.html_url),
            actor: None,
            reason: Some(self.reason),
            occurred_at: self.updated_at,
            raw_json,
        })
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GithubSubject {
    pub title: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: Option<String>,
    pub latest_comment_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GithubRepository {
    pub full_name: String,
    pub html_url: Option<String>,
}

impl serde::Serialize for GithubThread {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::Serialize;
        #[derive(Serialize)]
        struct Thread<'a> {
            id: &'a str,
            unread: bool,
            reason: &'a str,
            updated_at: DateTime<Utc>,
            subject: &'a GithubSubject,
            repository: &'a GithubRepository,
        }
        Thread {
            id: &self.id,
            unread: self.unread,
            reason: &self.reason,
            updated_at: self.updated_at,
            subject: &self.subject,
            repository: &self.repository,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderName, HeaderValue};

    use super::*;

    #[test]
    fn filters_github_notification_reasons() {
        let fixture = include_str!("../../fixtures/github/notifications.json");
        let threads: Vec<GithubThread> = serde_json::from_str(fixture).expect("fixture parses");

        let events = github_threads_to_event_drafts(7, threads).expect("events");

        assert_eq!(events.len(), 3);
        assert!(events.iter().all(|event| event.source == "github"));
        assert!(
            events
                .iter()
                .any(|event| event.reason.as_deref() == Some("mention"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.reason.as_deref() == Some("team_mention"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.reason.as_deref() == Some("review_requested"))
        );
    }

    #[test]
    fn parses_github_poll_interval_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-poll-interval"),
            HeaderValue::from_static("42"),
        );

        let before = Utc::now() + Duration::seconds(41);
        let poll_after = github_poll_after(&headers).expect("poll interval");
        let after = Utc::now() + Duration::seconds(43);

        assert!(poll_after >= before);
        assert!(poll_after <= after);
    }
}
