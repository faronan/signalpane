use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};

use crate::{
    collectors::{github::GithubCollector, slack::SlackCollector},
    config::{AppPaths, Config, Secrets},
    ipc::{IpcRequest, handle_request},
    store::Store,
};

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
            if let Err(err) = collect_once(&db_path, &config, &secrets) {
                let _ = append_log(&log_path, &format!("collector error: {err:#}"));
            }
            thread::sleep(Duration::from_secs(30));
        }
    });
}

fn collect_once(db_path: &Path, config: &Config, secrets: &Secrets) -> Result<()> {
    let store = Store::open(db_path)?;
    if config.github.enabled
        && let Some(token) = &secrets.github_token
    {
        let account_id =
            store.upsert_account("github", "GitHub", "default", true, &serde_json::json!({}))?;
        let cursor = store.get_cursor_state("github", "notifications")?;
        if !should_skip_poll(cursor.as_ref().and_then(|cursor| cursor.poll_after)) {
            let last_modified = cursor.and_then(|cursor| cursor.cursor_value);
            let outcome = GithubCollector::new(token.clone())
                .collect(account_id, last_modified.as_deref())
                .context("GitHub collector failed")?;
            let cursor_value = outcome.cursor_value.or(last_modified);
            let poll_after = outcome.poll_after.or_else(|| {
                Some(
                    Utc::now()
                        + ChronoDuration::seconds(config.github.poll_interval_seconds as i64),
                )
            });
            for event in outcome.events {
                store.upsert_event(&event)?;
            }
            store.set_cursor(
                "github",
                "notifications",
                cursor_value.as_deref(),
                poll_after,
                &serde_json::json!({}),
            )?;
        }
    }
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
            let outcome = collector
                .collect_channel(account_id, channel, oldest.as_deref())
                .with_context(|| format!("Slack collector failed for {channel}"))?;
            let poll_after = outcome.poll_after.or_else(|| {
                Some(
                    Utc::now() + ChronoDuration::seconds(config.slack.poll_interval_seconds as i64),
                )
            });
            for event in outcome.events {
                store.upsert_event(&event)?;
            }
            store.set_cursor(
                "slack",
                &cursor_key,
                outcome.cursor_value.as_deref(),
                poll_after,
                &serde_json::json!({ "channel": channel }),
            )?;
        }
    }
    Ok(())
}

fn should_skip_poll(poll_after: Option<chrono::DateTime<Utc>>) -> bool {
    poll_after.is_some_and(|poll_after| poll_after > Utc::now())
}

fn run_ipc_server(socket_path: &Path, db_path: &Path) -> Result<()> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove stale {}", socket_path.display()))?;
    }
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
