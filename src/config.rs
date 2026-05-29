use std::{
    env, fs,
    io::Write,
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

    pub fn init_file(path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        file.write_all(Self::default_toml()?.as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    pub fn default_toml() -> Result<String> {
        Self::default().to_toml()
    }

    pub fn to_toml(&self) -> Result<String> {
        let mut rendered = toml::to_string_pretty(self).context("failed to render config")?;
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        Ok(rendered)
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
        let rendered = Config::default_toml().expect("serialize default config");
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("SIGNALPANE_"));
    }

    #[test]
    fn partial_config_loads_with_defaults() {
        let config: Config = toml::from_str(
            r#"
[slack]
channels = ["C0123456789"]
"#,
        )
        .expect("parse partial config");

        assert_eq!(config.github, GithubConfig::default());
        assert!(config.slack.enabled);
        assert_eq!(config.slack.channels, vec!["C0123456789"]);
        assert_eq!(config.slack.poll_interval_seconds, 60);
    }

    #[test]
    fn init_file_creates_default_config_without_secrets() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("signalpane").join("config.toml");

        Config::init_file(&path).expect("init config");

        let rendered = fs::read_to_string(path).expect("read config");
        assert_eq!(
            toml::from_str::<Config>(&rendered).expect("parse config"),
            Config::default()
        );
        assert!(rendered.contains("[github]"));
        assert!(rendered.contains("[slack]"));
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("SIGNALPANE_"));
    }

    #[test]
    fn init_file_does_not_overwrite_existing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        fs::write(&path, "existing = true\n").expect("write existing config");

        let err = Config::init_file(&path).expect_err("existing config should fail");

        assert!(
            err.to_string().contains("failed to create"),
            "unexpected error: {err:#}"
        );
        assert_eq!(
            fs::read_to_string(path).expect("read existing config"),
            "existing = true\n"
        );
    }

    #[test]
    fn rendered_config_does_not_echo_unknown_secret_like_keys() {
        let config: Config = toml::from_str(
            r#"
github_token = "top-level-token"

[github]
enabled = false
token = "github-token"

[slack]
channels = ["C0123456789"]
secret = "slack-secret"
"#,
        )
        .expect("parse config with unknown keys");

        let rendered = config.to_toml().expect("render config");

        assert!(rendered.contains("enabled = false"));
        assert!(rendered.contains(r#"channels = ["C0123456789"]"#));
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("SIGNALPANE_"));
    }
}
