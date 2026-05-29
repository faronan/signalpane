#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    env, fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const GITHUB_TOKEN_ENV: &str = "SIGNALPANE_GITHUB_TOKEN";
pub const SLACK_USER_TOKEN_ENV: &str = "SIGNALPANE_SLACK_USER_TOKEN";
pub const SLACK_USER_ID_ENV: &str = "SIGNALPANE_SLACK_USER_ID";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub state_dir: PathBuf,
    pub db_file: PathBuf,
    pub socket_file: PathBuf,
    pub secrets_file: PathBuf,
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
            secrets_file: state_dir.join("secrets.env"),
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

#[derive(Clone, PartialEq, Eq)]
pub struct Secrets {
    pub github_token: Option<String>,
    pub slack_user_token: Option<String>,
    pub slack_user_id: Option<String>,
}

impl fmt::Debug for Secrets {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Secrets")
            .field("github_token", &presence_str(self.github_token.as_deref()))
            .field(
                "slack_user_token",
                &presence_str(self.slack_user_token.as_deref()),
            )
            .field(
                "slack_user_id",
                &presence_str(self.slack_user_id.as_deref()),
            )
            .finish()
    }
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            github_token: non_empty_env(GITHUB_TOKEN_ENV),
            slack_user_token: non_empty_env(SLACK_USER_TOKEN_ENV),
            slack_user_id: non_empty_env(SLACK_USER_ID_ENV),
        }
    }

    pub fn load(paths: &AppPaths) -> Result<Self> {
        let file_secrets = SecretsEnvFile::load(&paths.secrets_file)?.to_secrets();
        Ok(Self::from_sources(Self::from_env(), file_secrets))
    }

    pub fn set(paths: &AppPaths, key: SecretKey, value: &str) -> Result<()> {
        let mut file = SecretsEnvFile::load(&paths.secrets_file)?;
        file.upsert(key, value)?;
        file.write_to(&paths.secrets_file)
    }

    fn from_sources(env_secrets: Self, file_secrets: Self) -> Self {
        Self {
            github_token: env_secrets.github_token.or(file_secrets.github_token),
            slack_user_token: env_secrets
                .slack_user_token
                .or(file_secrets.slack_user_token),
            slack_user_id: env_secrets.slack_user_id.or(file_secrets.slack_user_id),
        }
    }

    pub fn value_for(&self, key: SecretKey) -> Option<&str> {
        match key {
            SecretKey::GithubToken => self.github_token.as_deref(),
            SecretKey::SlackUserToken => self.slack_user_token.as_deref(),
            SecretKey::SlackUserId => self.slack_user_id.as_deref(),
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

fn non_empty_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn presence_str(value: Option<&str>) -> &'static str {
    if value.is_some_and(|value| !value.trim().is_empty()) {
        "present"
    } else {
        "missing"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKey {
    GithubToken,
    SlackUserToken,
    SlackUserId,
}

impl SecretKey {
    pub const ALL: [Self; 3] = [Self::GithubToken, Self::SlackUserToken, Self::SlackUserId];

    pub fn env_name(self) -> &'static str {
        match self {
            Self::GithubToken => GITHUB_TOKEN_ENV,
            Self::SlackUserToken => SLACK_USER_TOKEN_ENV,
            Self::SlackUserId => SLACK_USER_ID_ENV,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            GITHUB_TOKEN_ENV => Some(Self::GithubToken),
            SLACK_USER_TOKEN_ENV => Some(Self::SlackUserToken),
            SLACK_USER_ID_ENV => Some(Self::SlackUserId),
            _ => None,
        }
    }
}

#[derive(Default)]
struct SecretsEnvFile {
    lines: Vec<SecretsEnvLine>,
}

impl fmt::Debug for SecretsEnvFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecretsEnvFile")
            .field("lines", &self.lines.len())
            .finish()
    }
}

enum SecretsEnvLine {
    Raw(String),
    Entry { key: SecretKey, value: String },
}

impl SecretsEnvFile {
    fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(raw) => Self::parse(&raw),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("failed to read {}", path.display())),
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        let mut lines = Vec::new();
        for (index, line) in raw.lines().enumerate() {
            let line_number = index + 1;
            let trimmed = line.trim();
            if trimmed.is_empty() || line.trim_start().starts_with('#') {
                lines.push(SecretsEnvLine::Raw(line.to_string()));
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                bail!("{}:{line_number}: expected KEY=VALUE", "secrets.env");
            };
            let key = key.trim();
            let Some(key) = SecretKey::parse(key) else {
                bail!("{}:{line_number}: unknown secret key {key}", "secrets.env");
            };
            lines.push(SecretsEnvLine::Entry {
                key,
                value: value.trim().to_string(),
            });
        }
        Ok(Self { lines })
    }

    fn to_secrets(&self) -> Secrets {
        let mut secrets = Secrets {
            github_token: None,
            slack_user_token: None,
            slack_user_id: None,
        };

        for line in &self.lines {
            let SecretsEnvLine::Entry { key, value } = line else {
                continue;
            };
            let value = non_empty_value(value);
            match key {
                SecretKey::GithubToken => secrets.github_token = value,
                SecretKey::SlackUserToken => secrets.slack_user_token = value,
                SecretKey::SlackUserId => secrets.slack_user_id = value,
            }
        }

        secrets
    }

    fn upsert(&mut self, key: SecretKey, value: &str) -> Result<()> {
        let value = value.trim();
        if value.is_empty() {
            bail!("{} value cannot be empty", key.env_name());
        }

        let mut found = false;
        for line in &mut self.lines {
            if let SecretsEnvLine::Entry {
                key: existing_key,
                value: existing_value,
            } = line
                && *existing_key == key
            {
                *existing_value = value.to_string();
                found = true;
            }
        }
        if !found {
            self.lines.push(SecretsEnvLine::Entry {
                key,
                value: value.to_string(),
            });
        }
        Ok(())
    }

    fn render(&self) -> String {
        let mut rendered = self
            .lines
            .iter()
            .map(|line| match line {
                SecretsEnvLine::Raw(value) => value.clone(),
                SecretsEnvLine::Entry { key, value } => {
                    format!("{}={value}", key.env_name())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !rendered.ends_with('\n') {
            rendered.push('\n');
        }
        rendered
    }

    #[cfg(unix)]
    fn write_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", path.display()))?;
        file.write_all(self.render().as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    #[cfg(not(unix))]
    fn write_to(&self, _path: &Path) -> Result<()> {
        bail!("signalpane secrets.env is only supported on Unix-like platforms")
    }
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
        assert_eq!(paths.secrets_file, PathBuf::from("/tmp/state/secrets.env"));
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

    #[test]
    fn parses_allowed_secrets_with_blank_lines_and_comments() {
        let file = SecretsEnvFile::parse(
            r#"
# local signalpane secrets
SIGNALPANE_GITHUB_TOKEN=ghp_test
SIGNALPANE_SLACK_USER_TOKEN = xoxp-test
SIGNALPANE_SLACK_USER_ID=U123
"#,
        )
        .expect("parse secrets file");

        assert_eq!(
            file.to_secrets(),
            Secrets {
                github_token: Some("ghp_test".to_string()),
                slack_user_token: Some("xoxp-test".to_string()),
                slack_user_id: Some("U123".to_string()),
            }
        );
    }

    #[test]
    fn empty_secret_values_are_missing() {
        let file = SecretsEnvFile::parse(concat!(
            "SIGNALPANE_GITHUB_TOKEN=\n",
            "SIGNALPANE_SLACK_USER_TOKEN=   \n",
            "SIGNALPANE_SLACK_USER_ID=U123\n",
        ))
        .expect("parse secrets file");

        assert_eq!(
            file.to_secrets(),
            Secrets {
                github_token: None,
                slack_user_token: None,
                slack_user_id: Some("U123".to_string()),
            }
        );
    }

    #[test]
    fn unknown_secret_key_is_error() {
        let err = SecretsEnvFile::parse("SIGNALPANE_OTHER_TOKEN=value\n")
            .expect_err("unknown key should fail");

        assert!(
            err.to_string().contains("unknown secret key"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn invalid_secret_line_is_error() {
        let err = SecretsEnvFile::parse("SIGNALPANE_GITHUB_TOKEN\n")
            .expect_err("invalid line should fail");

        assert!(
            err.to_string().contains("expected KEY=VALUE"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn env_secrets_take_precedence_over_file_secrets() {
        let merged = Secrets::from_sources(
            Secrets {
                github_token: Some("env-gh".to_string()),
                slack_user_token: None,
                slack_user_id: Some("env-user".to_string()),
            },
            Secrets {
                github_token: Some("file-gh".to_string()),
                slack_user_token: Some("file-slack".to_string()),
                slack_user_id: Some("file-user".to_string()),
            },
        );

        assert_eq!(
            merged,
            Secrets {
                github_token: Some("env-gh".to_string()),
                slack_user_token: Some("file-slack".to_string()),
                slack_user_id: Some("env-user".to_string()),
            }
        );
    }

    #[test]
    fn writes_secret_file_by_upserting_target_key_and_preserving_other_lines() {
        let mut file = SecretsEnvFile::parse(
            r#"# local
SIGNALPANE_GITHUB_TOKEN=old

SIGNALPANE_SLACK_USER_TOKEN=xoxp-old
"#,
        )
        .expect("parse secrets file");

        file.upsert(SecretKey::GithubToken, "new-token")
            .expect("upsert");

        assert_eq!(
            file.render(),
            "# local\nSIGNALPANE_GITHUB_TOKEN=new-token\n\nSIGNALPANE_SLACK_USER_TOKEN=xoxp-old\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn writes_secret_file_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state").join("secrets.env");
        let mut file = SecretsEnvFile::default();
        file.upsert(SecretKey::SlackUserToken, "xoxp-secret")
            .expect("upsert");

        file.write_to(&path).expect("write secrets file");

        let mode = fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
