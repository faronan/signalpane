use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::Secrets;

pub const DEFAULT_LOG_LINES: usize = 100;
pub const MAX_DAEMON_LOG_BYTES: u64 = 1024 * 1024;
pub const RETAIN_DAEMON_LOG_BYTES: u64 = 512 * 1024;
const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogTail {
    pub log_path: PathBuf,
    pub exists: bool,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCapOutcome {
    Missing,
    Unchanged {
        bytes: u64,
    },
    Capped {
        original_bytes: u64,
        retained_bytes: u64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretRedactor {
    values: Vec<String>,
}

impl SecretRedactor {
    pub fn from_secrets(secrets: &Secrets) -> Self {
        Self::from_values(
            [
                secrets.github_token.as_deref(),
                secrets.slack_user_token.as_deref(),
                secrets.slack_user_id.as_deref(),
            ]
            .into_iter()
            .flatten(),
        )
    }

    pub fn from_values<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut values = values
            .into_iter()
            .map(|value| value.as_ref().trim().to_string())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        values.sort();
        values.dedup();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self { values }
    }

    pub fn redact(&self, message: &str) -> String {
        let mut redacted = message.to_string();
        for value in &self.values {
            redacted = redacted.replace(value, REDACTED);
        }
        redacted
    }
}

pub fn read_log_tail(log_path: &Path, lines: usize) -> Result<LogTail> {
    let file = match fs::File::open(log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LogTail {
                log_path: log_path.to_path_buf(),
                exists: false,
                content: String::new(),
            });
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", log_path.display()));
        }
    };

    let mut tail = VecDeque::new();
    for line in BufReader::new(file).lines() {
        if lines == 0 {
            break;
        }
        if tail.len() == lines {
            tail.pop_front();
        }
        tail.push_back(line?);
    }

    let mut content = tail.into_iter().collect::<Vec<_>>().join("\n");
    if !content.is_empty() {
        content.push('\n');
    }

    Ok(LogTail {
        log_path: log_path.to_path_buf(),
        exists: true,
        content,
    })
}

pub fn append_log(path: &Path, message: &str, redactor: &SecretRedactor) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    writeln!(
        file,
        "{} {}",
        Utc::now().to_rfc3339(),
        redactor.redact(message)
    )
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn cap_log_size(
    path: &Path,
    max_bytes: u64,
    retain_bytes: u64,
    redactor: &SecretRedactor,
) -> Result<LogCapOutcome> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LogCapOutcome::Missing);
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let original_bytes = file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    if original_bytes <= max_bytes {
        return Ok(LogCapOutcome::Unchanged {
            bytes: original_bytes,
        });
    }

    let start = original_bytes.saturating_sub(retain_bytes);
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut retained = Vec::new();
    file.read_to_end(&mut retained)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if start > 0 {
        if let Some(index) = retained.iter().position(|byte| *byte == b'\n') {
            retained.drain(..=index);
        } else {
            retained.clear();
        }
    }

    let retained = redactor.redact(&String::from_utf8_lossy(&retained));
    let mut capped = format!(
        "{} log capped original_bytes={} max_bytes={} retained_target_bytes={}\n",
        Utc::now().to_rfc3339(),
        original_bytes,
        max_bytes,
        retain_bytes
    );
    capped.push_str(&retained);

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(capped.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))?;

    Ok(LogCapOutcome::Capped {
        original_bytes,
        retained_bytes: capped.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::config::Secrets;

    use super::*;

    #[test]
    fn missing_log_returns_empty_tail() {
        let dir = tempdir().expect("tempdir");

        let logs = read_log_tail(&dir.path().join("missing.log"), 100).expect("read log tail");

        assert!(!logs.exists);
        assert!(logs.content.is_empty());
    }

    #[test]
    fn reads_tail_of_existing_log() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        fs::write(&log_path, "one\ntwo\nthree\n").expect("write log");

        let logs = read_log_tail(&log_path, 2).expect("read log tail");

        assert!(logs.exists);
        assert_eq!(logs.content, "two\nthree\n");
    }

    #[test]
    fn cap_keeps_small_log_unchanged() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        fs::write(&log_path, "short\nlog\n").expect("write log");
        let redactor = SecretRedactor::default();

        let outcome = cap_log_size(&log_path, 1024, 512, &redactor).expect("cap log");

        assert_eq!(outcome, LogCapOutcome::Unchanged { bytes: 10 });
        assert_eq!(
            fs::read_to_string(&log_path).expect("read log"),
            "short\nlog\n"
        );
    }

    #[test]
    fn cap_large_log_in_place_and_marks_it() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        fs::write(&log_path, "old secret-token line\nkeep-1\nkeep-2\nkeep-3\n").expect("write log");
        let redactor = SecretRedactor::from_values(["secret-token"]);

        let outcome = cap_log_size(&log_path, 24, 16, &redactor).expect("cap log");
        let capped = fs::read_to_string(&log_path).expect("read capped log");

        assert_eq!(
            outcome,
            LogCapOutcome::Capped {
                original_bytes: 43,
                retained_bytes: capped.len() as u64,
            }
        );
        assert!(capped.contains("log capped original_bytes=43"));
        assert!(capped.contains("keep-2"));
        assert!(capped.contains("keep-3"));
        assert!(!capped.contains("keep-1"));
        assert!(!capped.contains("secret-token"));
    }

    #[test]
    fn append_log_redacts_known_secret_values() {
        let dir = tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        let redactor = SecretRedactor::from_secrets(&Secrets {
            github_token: Some("ghp_secret".to_string()),
            slack_user_token: Some("xoxp-secret".to_string()),
            slack_user_id: Some("U123".to_string()),
        });

        append_log(
            &log_path,
            "github=ghp_secret slack=xoxp-secret user=U123",
            &redactor,
        )
        .expect("append log");
        let log = fs::read_to_string(&log_path).expect("read log");

        assert!(log.contains("github=[REDACTED]"));
        assert!(log.contains("slack=[REDACTED]"));
        assert!(log.contains("user=[REDACTED]"));
        assert!(!log.contains("ghp_secret"));
        assert!(!log.contains("xoxp-secret"));
        assert!(!log.contains("U123"));
    }
}
