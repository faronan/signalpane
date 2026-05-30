use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Duration as ChronoDuration, Utc};

use crate::{
    collectors::CollectOutcome,
    collectors::{github::GithubCollector, slack::SlackCollector},
    config::{AppPaths, Config, Secrets},
    ipc::{IpcRequest, handle_request},
    logging::{self, LogCapOutcome, MAX_DAEMON_LOG_BYTES, RETAIN_DAEMON_LOG_BYTES, SecretRedactor},
    store::Store,
};

const COLLECTOR_LOOP_SECONDS: u64 = 30;
const FAILURE_BACKOFF_INITIAL_SECONDS: i64 = 60;
const FAILURE_BACKOFF_MAX_SECONDS: i64 = 15 * 60;

pub fn run_foreground(paths: AppPaths, config: Config, secrets: Secrets) -> Result<()> {
    paths.ensure_dirs()?;
    let redactor = SecretRedactor::from_secrets(&secrets);
    let shutdown = Arc::new(AtomicBool::new(false));
    install_shutdown_signal_handlers(&shutdown)?;

    let store = Store::open(&paths.db_file)?;
    bootstrap_accounts(&store, &config, &secrets)?;
    drop(store);

    let (listener, socket_guard) = bind_ipc_listener(&paths.socket_file)?;
    cap_startup_log(&paths.daemon_log, &redactor)?;
    logging::append_log(&paths.daemon_log, "daemon started", &redactor)?;

    let collector = spawn_collector_loop(
        paths.db_file.clone(),
        paths.daemon_log.clone(),
        config,
        secrets,
        Arc::clone(&shutdown),
        redactor.clone(),
    );
    let ipc_result = run_ipc_server(
        listener,
        socket_guard,
        paths.db_file.clone(),
        Arc::clone(&shutdown),
        paths.daemon_log.clone(),
        redactor.clone(),
    );
    shutdown.store(true, Ordering::SeqCst);
    join_collector_loop(collector, &paths.daemon_log, &redactor);
    let _ = logging::append_log(&paths.daemon_log, "daemon stopped", &redactor);
    ipc_result
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

fn install_shutdown_signal_handlers(shutdown: &Arc<AtomicBool>) -> Result<()> {
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(shutdown))
        .context("failed to register SIGINT handler")?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(shutdown))
        .context("failed to register SIGTERM handler")?;
    Ok(())
}

fn cap_startup_log(log_path: &Path, redactor: &SecretRedactor) -> Result<()> {
    match logging::cap_log_size(
        log_path,
        MAX_DAEMON_LOG_BYTES,
        RETAIN_DAEMON_LOG_BYTES,
        redactor,
    )? {
        LogCapOutcome::Capped {
            original_bytes,
            retained_bytes,
        } => logging::append_log(
            log_path,
            &format!(
                "daemon log capped original_bytes={original_bytes} retained_bytes={retained_bytes}"
            ),
            redactor,
        ),
        LogCapOutcome::Missing | LogCapOutcome::Unchanged { .. } => Ok(()),
    }
}

fn spawn_collector_loop(
    db_path: PathBuf,
    log_path: PathBuf,
    config: Config,
    secrets: Secrets,
    shutdown: Arc<AtomicBool>,
    redactor: SecretRedactor,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !shutdown.load(Ordering::SeqCst) {
            if let Err(err) = collect_once(&db_path, &log_path, &config, &secrets, &redactor) {
                let _ =
                    logging::append_log(&log_path, &format!("collector error: {err:#}"), &redactor);
            }
            sleep_until_shutdown(
                &shutdown,
                Duration::from_secs(COLLECTOR_LOOP_SECONDS),
                Duration::from_millis(200),
            );
        }
    })
}

fn join_collector_loop(collector: JoinHandle<()>, log_path: &Path, redactor: &SecretRedactor) {
    if collector.join().is_err() {
        let _ = logging::append_log(
            log_path,
            "collector thread panicked during shutdown",
            redactor,
        );
    }
}

fn sleep_until_shutdown(shutdown: &AtomicBool, duration: Duration, step: Duration) {
    let mut slept = Duration::ZERO;
    while slept < duration && !shutdown.load(Ordering::SeqCst) {
        let remaining = duration.saturating_sub(slept);
        let sleep_for = remaining.min(step);
        thread::sleep(sleep_for);
        slept += sleep_for;
    }
}

fn collect_once(
    db_path: &Path,
    log_path: &Path,
    config: &Config,
    secrets: &Secrets,
    redactor: &SecretRedactor,
) -> Result<()> {
    let store = Store::open(db_path)?;
    collect_github(&store, log_path, config, secrets, redactor)?;
    collect_slack(&store, log_path, config, secrets, redactor)?;
    Ok(())
}

fn collect_github(
    store: &Store,
    log_path: &Path,
    config: &Config,
    secrets: &Secrets,
    redactor: &SecretRedactor,
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
                    redactor,
                )?;
            }
        }
    }
    Ok(())
}

fn collect_slack(
    store: &Store,
    log_path: &Path,
    config: &Config,
    secrets: &Secrets,
    redactor: &SecretRedactor,
) -> Result<()> {
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
                record_collector_failure(
                    store,
                    log_path,
                    "slack",
                    &cursor_key,
                    &metadata,
                    &err,
                    redactor,
                )?;
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
    redactor: &SecretRedactor,
) -> Result<()> {
    let consecutive_failures = store
        .get_cursor_state(source, cursor_key)?
        .map(|cursor| cursor.consecutive_failures.saturating_add(1))
        .unwrap_or(1)
        .max(1);
    let poll_after = Some(Utc::now() + failure_backoff_duration(consecutive_failures));
    let error = redactor.redact(&compact_error_message(&format!("{err:#}")));
    let store_result = store.record_cursor_failure(
        source,
        cursor_key,
        &error,
        consecutive_failures,
        poll_after,
        metadata,
    );
    let log_result = logging::append_log(
        log_path,
        &format!("{source}/{cursor_key} collector error failures={consecutive_failures}: {error}"),
        redactor,
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

fn bind_ipc_listener(socket_path: &Path) -> Result<(UnixListener, SocketGuard)> {
    prepare_socket_path(socket_path)?;
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    listener
        .set_nonblocking(true)
        .context("failed to configure IPC listener as nonblocking")?;
    Ok((listener, SocketGuard::new(socket_path.to_path_buf())))
}

fn run_ipc_server(
    listener: UnixListener,
    _socket_guard: SocketGuard,
    db_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    log_path: PathBuf,
    redactor: SecretRedactor,
) -> Result<()> {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _addr)) => {
                let db_path = db_path.to_path_buf();
                thread::spawn(move || {
                    let _ = handle_stream(stream, &db_path);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => {
                logging::append_log(
                    &log_path,
                    &format!("signalpane ipc accept error: {err}"),
                    &redactor,
                )?;
            }
        }
    }
    Ok(())
}

struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
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
    fn socket_guard_removes_bound_socket_on_drop() {
        let dir = tempdir().expect("tempdir");
        let socket_path = dir.path().join("signalpane.sock");

        let (_listener, guard) = bind_ipc_listener(&socket_path).expect("bind ipc listener");
        assert!(socket_path.exists());
        drop(guard);

        assert!(!socket_path.exists());
    }

    #[test]
    fn ipc_server_exits_on_shutdown_and_cleans_socket() {
        let dir = tempdir().expect("tempdir");
        let db_path = dir.path().join("signalpane.sqlite3");
        Store::open(&db_path).expect("store");
        let socket_path = dir.path().join("signalpane.sock");
        let log_path = dir.path().join("daemon.log");
        let (listener, guard) = bind_ipc_listener(&socket_path).expect("bind ipc listener");
        let shutdown = Arc::new(AtomicBool::new(false));
        let server_shutdown = Arc::clone(&shutdown);

        let handle = thread::spawn(move || {
            run_ipc_server(
                listener,
                guard,
                db_path,
                server_shutdown,
                log_path,
                SecretRedactor::default(),
            )
            .expect("ipc server");
        });
        thread::sleep(Duration::from_millis(100));
        shutdown.store(true, Ordering::SeqCst);

        handle.join().expect("join server");
        assert!(!socket_path.exists());
    }

    #[test]
    fn collector_failure_redacts_secret_in_cursor_metadata_and_log() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        let store = Store::open_in_memory().expect("store");
        let redactor = SecretRedactor::from_values(["ghp_secret"]);

        record_collector_failure(
            &store,
            &log_path,
            "github",
            "notifications",
            &serde_json::json!({}),
            &anyhow::anyhow!("request failed with ghp_secret"),
            &redactor,
        )
        .expect("record failure");

        let cursor = store
            .get_cursor_state("github", "notifications")
            .expect("cursor query")
            .expect("cursor");
        let log = fs::read_to_string(&log_path).expect("read log");

        assert_eq!(
            cursor.last_error.as_deref(),
            Some("request failed with [REDACTED]")
        );
        assert!(log.contains("request failed with [REDACTED]"));
        assert!(!log.contains("ghp_secret"));
    }

    #[test]
    fn failure_backoff_exponentially_increases_and_caps() {
        assert_eq!(failure_backoff_duration(1), ChronoDuration::seconds(60));
        assert_eq!(failure_backoff_duration(2), ChronoDuration::seconds(120));
        assert_eq!(failure_backoff_duration(99), ChronoDuration::seconds(900));
    }
}
