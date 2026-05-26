use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    pub db_file: PathBuf,
    pub socket_file: PathBuf,
    pub log_dir: PathBuf,
    pub daemon_log: PathBuf,
}

impl AppPaths {
    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is required to resolve signalpane paths")?;
        let config_base = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state_base = env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/state"));
        Ok(Self::from_bases(
            config_base.join("signalpane"),
            state_base.join("signalpane"),
        ))
    }

    pub fn from_bases(config_dir: PathBuf, state_dir: PathBuf) -> Self {
        let log_dir = state_dir.join("logs");
        Self {
            config_file: config_dir.join("config.toml"),
            db_file: state_dir.join("signalpane.sqlite3"),
            socket_file: state_dir.join("signalpane.sock"),
            daemon_log: log_dir.join("daemon.log"),
            config_dir,
            state_dir,
            log_dir,
        }
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("failed to create {}", self.config_dir.display()))?;
        fs::create_dir_all(&self.state_dir)
            .with_context(|| format!("failed to create {}", self.state_dir.display()))?;
        fs::create_dir_all(&self.log_dir)
            .with_context(|| format!("failed to create {}", self.log_dir.display()))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub slack: SlackConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GithubConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
}

impl Default for GithubConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_seconds: default_poll_interval(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub channels: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_seconds: u64,
}

impl Default for SlackConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            channels: Vec::new(),
            poll_interval_seconds: default_poll_interval(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u64 {
    60
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secrets {
    pub github_token: Option<String>,
    pub slack_user_token: Option<String>,
    pub slack_user_id: Option<String>,
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            github_token: non_empty_env("SIGNALPANE_GITHUB_TOKEN"),
            slack_user_token: non_empty_env("SIGNALPANE_SLACK_USER_TOKEN"),
            slack_user_id: non_empty_env("SIGNALPANE_SLACK_USER_ID"),
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_xdg_style_paths() {
        let paths = AppPaths::from_bases(PathBuf::from("/tmp/cfg"), PathBuf::from("/tmp/state"));
        assert_eq!(paths.config_file, PathBuf::from("/tmp/cfg/config.toml"));
        assert_eq!(
            paths.db_file,
            PathBuf::from("/tmp/state/signalpane.sqlite3")
        );
        assert_eq!(
            paths.socket_file,
            PathBuf::from("/tmp/state/signalpane.sock")
        );
        assert_eq!(
            paths.daemon_log,
            PathBuf::from("/tmp/state/logs/daemon.log")
        );
    }

    #[test]
    fn config_defaults_do_not_contain_secret_fields() {
        let rendered = toml::to_string(&Config::default()).expect("serialize default config");
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("secret"));
    }
}
