use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Duration as ChronoDuration, Utc};

use crate::{
    collectors::CollectOutcome,
    collectors::{github::GithubCollector, slack::SlackCollector},
    config::{AppPaths, Config, Secrets},
    ipc::{IpcRequest, handle_request},
    store::Store,
};

const COLLECTOR_LOOP_SECONDS: u64 = 30;
const FAILURE_BACKOFF_INITIAL_SECONDS: i64 = 60;
const FAILURE_BACKOFF_MAX_SECONDS: i64 = 15 * 60;

pub fn run_foreground(paths: AppPaths, config: Config, secrets: Secrets) -> Result<()> {
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_file)?;
    bootstrap_accounts(&store, &config, &secrets)?;
    drop(store);

    spawn_collector_loop(
        paths.db_file.clone(),
        paths.daemon_log.clone(),
        config,
        secrets,
    );
    run_ipc_server(&paths.socket_file, &paths.db_file)
}

fn bootstrap_accounts(store: &Store, config: &Config, secrets: &Secrets) -> Result<()> {
    if config.github.enabled && secrets.github_token.is_some() {
        store.upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))?;
    }
    if config.slack.enabled && secrets.slack_user_token.is_some() {
        store.upsert_account(
            "slack",
            "Slack",
            "default",
            true,
            &serde_json::json!({
                "channels": config.slack.channels,
            }),
        )?;
    }
    Ok(())
}

fn spawn_collector_loop(db_path: PathBuf, log_path: PathBuf, config: Config, secrets: Secrets) {
    thread::spawn(move || {
        loop {
            if let Err(err) = collect_once(&db_path, &log_path, &config, &secrets) {
                let _ = append_log(&log_path, &format!("collector error: {err:#}"));
            }
            thread::sleep(Duration::from_secs(COLLECTOR_LOOP_SECONDS));
        }
    });
}

fn collect_once(db_path: &Path, log_path: &Path, config: &Config, secrets: &Secrets) -> Result<()> {
    let store = Store::open(db_path)?;
    collect_github(&store, log_path, config, secrets)?;
    collect_slack(&store, log_path, config, secrets)?;
    Ok(())
}

fn collect_github(
    store: &Store,
    log_path: &Path,
    config: &Config,
    secrets: &Secrets,
) -> Result<()> {
    if config.github.enabled
        && let Some(token) = &secrets.github_token
    {
        let account_id =
            store.upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))?;
        let cursor = store.get_cursor_state("github", "notifications")?;
        if !should_skip_poll(cursor.as_ref().and_then(|cursor| cursor.poll_after)) {
            let last_modified = cursor.and_then(|cursor| cursor.cursor_value);
            let result = (|| -> Result<()> {
                let outcome = GithubCollector::new(token.clone())
                    .collect(account_id, last_modified.as_deref())
                    .context("GitHub collector failed")?;
                persist_successful_outcome(
                    store,
                    "github",
                    "notifications",
                    outcome,
                    last_modified,
                    config.github.poll_interval_seconds,
                    &serde_json::json!({}),
                )
            })();
            if let Err(err) = result {
                record_collector_failure(
                    store,
                    log_path,
                    "github",
                    "notifications",
                    &serde_json::json!({}),
                    &err,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_slack(store: &Store, log_path: &Path, config: &Config, secrets: &Secrets) -> Result<()> {
    if config.slack.enabled
        && let Some(token) = &secrets.slack_user_token
        && !config.slack.channels.is_empty()
    {
        let account_id = store.upsert_account(
            "slack",
            "Slack",
            "default",
            true,
            &serde_json::json!({ "channels": config.slack.channels }),
        )?;
        let collector = SlackCollector::new(token.clone(), secrets.slack_user_id.clone());
        for channel in &config.slack.channels {
            let cursor_key = format!("channel:{channel}");
            let cursor = store.get_cursor_state("slack", &cursor_key)?;
            if should_skip_poll(cursor.as_ref().and_then(|cursor| cursor.poll_after)) {
                continue;
            }
            let oldest = cursor.and_then(|cursor| cursor.cursor_value);
            let metadata = serde_json::json!({ "channel": channel });
            let result = (|| -> Result<()> {
                let outcome = collector
                    .collect_channel(account_id, channel, oldest.as_deref())
                    .with_context(|| format!("Slack collector failed for {channel}"))?;
                persist_successful_outcome(
                    store,
                    "slack",
                    &cursor_key,
                    outcome,
                    oldest,
                    config.slack.poll_interval_seconds,
                    &metadata,
                )
            })();
            if let Err(err) = result {
                record_collector_failure(store, log_path, "slack", &cursor_key, &metadata, &err)?;
            }
        }
    }
    Ok(())
}

fn persist_successful_outcome(
    store: &Store,
    source: &str,
    cursor_key: &str,
    outcome: CollectOutcome,
    fallback_cursor: Option<String>,
    poll_interval_seconds: u64,
    metadata: &serde_json::Value,
) -> Result<()> {
    let cursor_value = outcome.cursor_value.or(fallback_cursor);
    let poll_after = outcome
        .poll_after
        .or_else(|| Some(Utc::now() + ChronoDuration::seconds(poll_interval_seconds as i64)));
    for event in outcome.events {
        store.upsert_event(&event)?;
    }
    store.record_cursor_success(
        source,
        cursor_key,
        cursor_value.as_deref(),
        poll_after,
        metadata,
    )
}

fn record_collector_failure(
    store: &Store,
    log_path: &Path,
    source: &str,
    cursor_key: &str,
    metadata: &serde_json::Value,
    err: &anyhow::Error,
) -> Result<()> {
    let consecutive_failures = store
        .get_cursor_state(source, cursor_key)?
        .map(|cursor| cursor.consecutive_failures.saturating_add(1))
        .unwrap_or(1)
        .max(1);
    let poll_after = Some(Utc::now() + failure_backoff_duration(consecutive_failures));
    let error = compact_error_message(&format!("{err:#}"));
    let store_result = store.record_cursor_failure(
        source,
        cursor_key,
        &error,
        consecutive_failures,
        poll_after,
        metadata,
    );
    let log_result = append_log(
        log_path,
        &format!("{source}/{cursor_key} collector error failures={consecutive_failures}: {error}"),
    );
    store_result?;
    log_result
}

fn should_skip_poll(poll_after: Option<chrono::DateTime<Utc>>) -> bool {
    poll_after.is_some_and(|poll_after| poll_after > Utc::now())
}

fn failure_backoff_duration(consecutive_failures: u32) -> ChronoDuration {
    let exponent = consecutive_failures.saturating_sub(1).min(31);
    let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    let seconds = FAILURE_BACKOFF_INITIAL_SECONDS
        .saturating_mul(multiplier)
        .min(FAILURE_BACKOFF_MAX_SECONDS);
    ChronoDuration::seconds(seconds)
}

fn compact_error_message(message: &str) -> String {
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 512 {
        return compact;
    }
    let mut truncated = compact.chars().take(509).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn run_ipc_server(socket_path: &Path, db_path: &Path) -> Result<()> {
    prepare_socket_path(socket_path)?;
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let db_path = db_path.to_path_buf();
                thread::spawn(move || {
                    let _ = handle_stream(stream, &db_path);
                });
            }
            Err(err) => eprintln!("signalpane ipc accept error: {err}"),
        }
    }
    Ok(())
}

fn prepare_socket_path(socket_path: &Path) -> Result<()> {
    if !socket_path.exists() {
        return Ok(());
    }
    if UnixStream::connect(socket_path).is_ok() {
        bail!(
            "signalpane daemon already running at {}",
            socket_path.display()
        );
    }
    fs::remove_file(socket_path)
        .with_context(|| format!("failed to remove stale {}", socket_path.display()))?;
    Ok(())
}

fn handle_stream(mut stream: UnixStream, db_path: &Path) -> Result<()> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line)?;
    }
    let request: IpcRequest = serde_json::from_str(line.trim_end())?;
    let response = handle_request(db_path, request);
    let payload = serde_json::to_string(&response)?;
    stream.write_all(payload.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn append_log(path: &Path, message: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{} {message}", Utc::now().to_rfc3339())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn stale_socket_is_removed_before_bind() {
        let dir = tempdir().expect("tempdir");
        let socket_path = dir.path().join("signalpane.sock");
        let listener = UnixListener::bind(&socket_path).expect("bind stale listener");
        drop(listener);

        prepare_socket_path(&socket_path).expect("prepare stale socket");

        assert!(!socket_path.exists());
    }

    #[test]
    fn live_socket_is_not_removed() {
        let dir = tempdir().expect("tempdir");
        let socket_path = dir.path().join("signalpane.sock");
        let _listener = UnixListener::bind(&socket_path).expect("bind live listener");

        let err = prepare_socket_path(&socket_path).expect_err("live socket should fail");

        assert!(socket_path.exists());
        assert!(
            err.to_string().contains("already running"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn failure_backoff_exponentially_increases_and_caps() {
        assert_eq!(failure_backoff_duration(1), ChronoDuration::seconds(60));
        assert_eq!(failure_backoff_duration(2), ChronoDuration::seconds(120));
        assert_eq!(failure_backoff_duration(99), ChronoDuration::seconds(900));
    }
}
