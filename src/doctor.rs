use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{
    config::{
        AppPaths, Config, GITHUB_TOKEN_ENV, SLACK_USER_ID_ENV, SLACK_USER_TOKEN_ENV, Secrets,
    },
    ipc::IpcClient,
    launch_agent::{self, DaemonIpcStatus, LaunchAgentStatus},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    config_path: PathBuf,
    config_exists: bool,
    config_parse: ParseStatus,
    github_enabled: Option<bool>,
    github_token_present: bool,
    github_status: SourceReadiness,
    slack_enabled: Option<bool>,
    slack_user_token_present: bool,
    slack_user_id_present: bool,
    slack_status: SourceReadiness,
    launch_agent_status: CheckStatus,
    launch_agent_loaded: Option<bool>,
    daemon_ipc: DaemonIpcStatus,
    socket_path: PathBuf,
    socket_exists: bool,
    log_path: PathBuf,
    log_exists: bool,
    db_path: PathBuf,
    db_exists: bool,
    plist_path: Option<PathBuf>,
    plist_exists: Option<bool>,
    registered_binary_path: Option<PathBuf>,
    current_binary_path: Option<PathBuf>,
    binary_path_match: Option<bool>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

impl DoctorReport {
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn to_key_value(&self) -> String {
        let mut output = String::new();
        push_kv(
            &mut output,
            "overall_status",
            self.overall_status().as_str(),
        );
        push_kv(&mut output, "config_path", self.config_path.display());
        push_kv(&mut output, "config_exists", self.config_exists);
        push_kv(&mut output, "config_parse", self.config_parse.as_str());
        push_kv(
            &mut output,
            "github_enabled",
            format_optional_bool(self.github_enabled),
        );
        push_kv(
            &mut output,
            "github_token_present",
            self.github_token_present,
        );
        push_kv(&mut output, "github_status", self.github_status.as_str());
        push_kv(
            &mut output,
            "slack_enabled",
            format_optional_bool(self.slack_enabled),
        );
        push_kv(
            &mut output,
            "slack_user_token_present",
            self.slack_user_token_present,
        );
        push_kv(
            &mut output,
            "slack_user_id_present",
            self.slack_user_id_present,
        );
        push_kv(&mut output, "slack_status", self.slack_status.as_str());
        push_kv(
            &mut output,
            "launch_agent_status",
            self.launch_agent_status.as_str(),
        );
        push_kv(
            &mut output,
            "launch_agent_loaded",
            format_optional_bool(self.launch_agent_loaded),
        );
        push_kv(&mut output, "daemon_ipc", self.daemon_ipc.as_str());
        push_kv(&mut output, "socket_path", self.socket_path.display());
        push_kv(&mut output, "socket_exists", self.socket_exists);
        push_kv(&mut output, "log_path", self.log_path.display());
        push_kv(&mut output, "log_exists", self.log_exists);
        push_kv(&mut output, "db_path", self.db_path.display());
        push_kv(&mut output, "db_exists", self.db_exists);
        push_kv(
            &mut output,
            "plist_path",
            format_optional_path(self.plist_path.as_ref()),
        );
        push_kv(
            &mut output,
            "plist_exists",
            format_optional_bool(self.plist_exists),
        );
        push_kv(
            &mut output,
            "registered_binary_path",
            format_optional_path(self.registered_binary_path.as_ref()),
        );
        push_kv(
            &mut output,
            "current_binary_path",
            format_optional_path(self.current_binary_path.as_ref()),
        );
        push_kv(
            &mut output,
            "binary_path_match",
            format_optional_bool(self.binary_path_match),
        );
        for warning in &self.warnings {
            push_kv(&mut output, "warning", warning);
        }
        for error in &self.errors {
            push_kv(&mut output, "error", error);
        }
        output
    }

    fn overall_status(&self) -> OverallStatus {
        if !self.errors.is_empty() {
            OverallStatus::Error
        } else if !self.warnings.is_empty() {
            OverallStatus::Warning
        } else {
            OverallStatus::Ok
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverallStatus {
    Ok,
    Warning,
    Error,
}

impl OverallStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseStatus {
    Ok,
    Error,
}

impl ParseStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceReadiness {
    Ok,
    Warning,
    Error,
    Disabled,
    Unknown,
}

impl SourceReadiness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Disabled => "disabled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Error,
}

impl CheckStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
}

pub fn check(paths: &AppPaths) -> DoctorReport {
    check_with_probe(paths, &SystemDoctorProbe)
}

trait DoctorProbe {
    fn secrets(&self, paths: &AppPaths) -> Result<Secrets>;
    fn current_binary_path(&self) -> Result<PathBuf>;
    fn launch_agent_status(&self, paths: &AppPaths) -> Result<LaunchAgentStatus>;
    fn daemon_ipc_status(&self, socket_path: &Path) -> DaemonIpcStatus;
}

struct SystemDoctorProbe;

impl DoctorProbe for SystemDoctorProbe {
    fn secrets(&self, paths: &AppPaths) -> Result<Secrets> {
        Secrets::load(paths)
    }

    fn current_binary_path(&self) -> Result<PathBuf> {
        env::current_exe().context("failed to resolve current executable")
    }

    fn launch_agent_status(&self, paths: &AppPaths) -> Result<LaunchAgentStatus> {
        launch_agent::status(paths)
    }

    fn daemon_ipc_status(&self, socket_path: &Path) -> DaemonIpcStatus {
        if IpcClient::new(socket_path.to_path_buf()).status().is_ok() {
            DaemonIpcStatus::Responsive
        } else {
            DaemonIpcStatus::Unreachable
        }
    }
}

fn check_with_probe(paths: &AppPaths, probe: &impl DoctorProbe) -> DoctorReport {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let config_exists = paths.config_file.exists();
    let (config_parse, config) = match load_effective_config(&paths.config_file) {
        Ok(config) => {
            if !config_exists {
                warnings.push(format!(
                    "config file does not exist at {}; using defaults",
                    paths.config_file.display()
                ));
            }
            (ParseStatus::Ok, Some(config))
        }
        Err(err) => {
            errors.push(format!("config parse failed: {err:#}"));
            (ParseStatus::Error, None)
        }
    };

    let secrets = match probe.secrets(paths) {
        Ok(secrets) => Some(secrets),
        Err(err) => {
            errors.push(format!("secrets load failed: {err:#}"));
            None
        }
    };
    let github_token_present = secrets
        .as_ref()
        .is_some_and(|secrets| secrets.github_token.is_some());
    let slack_user_token_present = secrets
        .as_ref()
        .is_some_and(|secrets| secrets.slack_user_token.is_some());
    let slack_user_id_present = secrets
        .as_ref()
        .is_some_and(|secrets| secrets.slack_user_id.is_some());

    let (github_enabled, github_status) = match config.as_ref() {
        Some(config) if !config.github.enabled => (Some(false), SourceReadiness::Disabled),
        Some(_) if !github_token_present => {
            errors.push(format!(
                "github is enabled but {GITHUB_TOKEN_ENV} is not present"
            ));
            (Some(true), SourceReadiness::Error)
        }
        Some(_) => (Some(true), SourceReadiness::Ok),
        None => (None, SourceReadiness::Unknown),
    };

    let (slack_enabled, slack_status) = match config.as_ref() {
        Some(config) if !config.slack.enabled => (Some(false), SourceReadiness::Disabled),
        Some(_) if !slack_user_token_present => {
            errors.push(format!(
                "slack is enabled but {SLACK_USER_TOKEN_ENV} is not present"
            ));
            (Some(true), SourceReadiness::Error)
        }
        Some(_) if !slack_user_id_present => {
            warnings.push(format!(
                "slack is enabled but {SLACK_USER_ID_ENV} is not present; runtime will resolve it with auth.test"
            ));
            (Some(true), SourceReadiness::Warning)
        }
        Some(_) => (Some(true), SourceReadiness::Ok),
        None => (None, SourceReadiness::Unknown),
    };

    let socket_path = paths.socket_file.clone();
    let log_path = paths.daemon_log.clone();
    let db_path = paths.db_file.clone();
    let socket_exists = socket_path.exists();
    let log_exists = log_path.exists();
    let db_exists = db_path.exists();

    let current_binary_path = match probe.current_binary_path() {
        Ok(path) => Some(path),
        Err(err) => {
            errors.push(format!("current binary path unavailable: {err:#}"));
            None
        }
    };

    let fallback_plist_path = launch_agent::launch_agent_path_from_env().ok();
    let mut daemon_ipc = probe.daemon_ipc_status(&socket_path);
    let (
        launch_agent_status,
        launch_agent_loaded,
        plist_path,
        plist_exists,
        registered_binary_path,
    ) = match probe.launch_agent_status(paths) {
        Ok(status) => {
            daemon_ipc = status.daemon_ipc;
            if !status.loaded {
                warnings.push(format!("LaunchAgent {} is not loaded", launch_agent::LABEL));
            }
            for warning in status.warnings {
                push_unique(&mut warnings, warning);
            }
            let plist_exists = status.plist_path.exists();
            let registered_binary_path = plist_exists.then_some(status.binary_path);
            (
                CheckStatus::Ok,
                Some(status.loaded),
                Some(status.plist_path),
                Some(plist_exists),
                registered_binary_path,
            )
        }
        Err(err) => {
            errors.push(format!("LaunchAgent status unavailable: {err:#}"));
            let plist_exists = fallback_plist_path.as_ref().map(|path| path.exists());
            (
                CheckStatus::Error,
                None,
                fallback_plist_path,
                plist_exists,
                None,
            )
        }
    };

    if daemon_ipc == DaemonIpcStatus::Unreachable {
        errors.push(format!(
            "daemon IPC is unreachable at {}",
            socket_path.display()
        ));
    }

    let binary_path_match = match (&registered_binary_path, &current_binary_path) {
        (Some(registered), Some(current)) => Some(registered == current),
        _ => None,
    };
    if binary_path_match == Some(false) {
        push_unique(
            &mut warnings,
            "registered binary differs from current executable".to_string(),
        );
    }

    DoctorReport {
        config_path: paths.config_file.clone(),
        config_exists,
        config_parse,
        github_enabled,
        github_token_present,
        github_status,
        slack_enabled,
        slack_user_token_present,
        slack_user_id_present,
        slack_status,
        launch_agent_status,
        launch_agent_loaded,
        daemon_ipc,
        socket_path,
        socket_exists,
        log_path,
        log_exists,
        db_path,
        db_exists,
        plist_path,
        plist_exists,
        registered_binary_path,
        current_binary_path,
        binary_path_match,
        warnings,
        errors,
    }
}

fn load_effective_config(path: &Path) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn push_kv(output: &mut String, key: &str, value: impl ToString) {
    output.push_str(key);
    output.push('=');
    output.push_str(&sanitize_cli_value(&value.to_string()));
    output.push('\n');
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn format_optional_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    }
}

fn format_optional_path(value: Option<&PathBuf>) -> String {
    value
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn sanitize_cli_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            ch => ch,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::bail;
    use tempfile::tempdir;

    use super::*;

    #[derive(Debug, Clone)]
    struct FakeProbe {
        secrets: Secrets,
        current_binary_path: Option<PathBuf>,
        launch_agent_status: Result<LaunchAgentStatus, &'static str>,
        daemon_ipc: DaemonIpcStatus,
    }

    impl DoctorProbe for FakeProbe {
        fn secrets(&self, _paths: &AppPaths) -> Result<Secrets> {
            Ok(self.secrets.clone())
        }

        fn current_binary_path(&self) -> Result<PathBuf> {
            self.current_binary_path
                .clone()
                .context("fake current binary path missing")
        }

        fn launch_agent_status(&self, _paths: &AppPaths) -> Result<LaunchAgentStatus> {
            match &self.launch_agent_status {
                Ok(status) => Ok(status.clone()),
                Err(message) => bail!(*message),
            }
        }

        fn daemon_ipc_status(&self, _socket_path: &Path) -> DaemonIpcStatus {
            self.daemon_ipc
        }
    }

    #[test]
    fn formats_strict_key_value_lines() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
[github]
enabled = false

[slack]
enabled = false
"#,
        );
        let probe = fake_probe(&paths);

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(&output, "overall_status=ok");
        for line in output.lines() {
            assert!(
                line.contains('='),
                "doctor output line is not key=value: {line:?}"
            );
            assert!(!line.contains('\t'));
            assert!(!line.contains('\r'));
        }
    }

    #[test]
    fn missing_config_uses_defaults_but_reports_missing_file() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        let mut probe = fake_probe(&paths);
        probe.secrets = all_secrets();

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(&output, "config_exists=false");
        assert_line(&output, "config_parse=ok");
        assert_line(&output, "github_enabled=true");
        assert_line(&output, "slack_enabled=true");
        assert_line_contains(&output, "warning=config file does not exist");
    }

    #[test]
    fn invalid_config_reports_parse_error_without_panicking() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(&paths, "[github\nbroken = true\n");
        let probe = fake_probe(&paths);

        let report = check_with_probe(&paths, &probe);
        let output = report.to_key_value();

        assert!(report.has_errors());
        assert_line(&output, "overall_status=error");
        assert_line(&output, "config_parse=error");
        assert_line(&output, "github_enabled=unknown");
        assert_line_contains(&output, "error=config parse failed:");
    }

    #[test]
    fn enabled_github_without_token_is_error() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
[github]
enabled = true

[slack]
enabled = false
"#,
        );
        let probe = fake_probe(&paths);

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(&output, "github_status=error");
        assert_line_contains(
            &output,
            "error=github is enabled but SIGNALPANE_GITHUB_TOKEN is not present",
        );
    }

    #[test]
    fn enabled_slack_without_user_token_is_error() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
[github]
enabled = false

[slack]
enabled = true
"#,
        );
        let probe = fake_probe(&paths);

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(&output, "slack_status=error");
        assert_line_contains(
            &output,
            "error=slack is enabled but SIGNALPANE_SLACK_USER_TOKEN is not present",
        );
    }

    #[test]
    fn enabled_slack_without_user_id_is_warning() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
[github]
enabled = false

[slack]
enabled = true
"#,
        );
        let mut probe = fake_probe(&paths);
        probe.secrets = Secrets {
            github_token: None,
            slack_user_token: Some("xoxp-secret".to_string()),
            slack_user_id: None,
        };

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(&output, "overall_status=warning");
        assert_line(&output, "slack_status=warning");
        assert_line_contains(
            &output,
            "warning=slack is enabled but SIGNALPANE_SLACK_USER_ID is not present",
        );
    }

    #[test]
    fn binary_path_mismatch_is_surfaced_without_mutating_launch_agent() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
[github]
enabled = false

[slack]
enabled = false
"#,
        );
        let plist_path = dir.path().join("com.faronan.signalpane.plist");
        fs::write(&plist_path, "plist").expect("write plist");
        let mut probe = fake_probe(&paths);
        probe.current_binary_path = Some(PathBuf::from("/tmp/debug/signalpane"));
        probe.launch_agent_status = Ok(LaunchAgentStatus {
            label: launch_agent::LABEL,
            loaded: true,
            daemon_ipc: DaemonIpcStatus::Responsive,
            socket_path: paths.socket_file.clone(),
            log_path: paths.daemon_log.clone(),
            plist_path: plist_path.clone(),
            binary_path: PathBuf::from("/Users/alice/.local/bin/signalpane"),
            warnings: Vec::new(),
        });

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert_line(
            &output,
            "registered_binary_path=/Users/alice/.local/bin/signalpane",
        );
        assert_line(&output, "current_binary_path=/tmp/debug/signalpane");
        assert_line(&output, "binary_path_match=false");
        assert_line_contains(
            &output,
            "warning=registered binary differs from current executable",
        );
    }

    #[test]
    fn token_like_values_are_never_rendered() {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        write_config(
            &paths,
            r#"
github_token = "top-level-secret"

[github]
enabled = true
token = "ghp_secret"

[slack]
enabled = true
secret = "xoxp-secret"
"#,
        );
        let mut probe = fake_probe(&paths);
        probe.secrets = all_secrets();

        let output = check_with_probe(&paths, &probe).to_key_value();

        assert!(!output.contains("top-level-secret"));
        assert!(!output.contains("ghp_secret"));
        assert!(!output.contains("xoxp-secret"));
    }

    fn fake_probe(paths: &AppPaths) -> FakeProbe {
        let plist_path = paths
            .state_dir
            .join("LaunchAgents")
            .join("com.faronan.signalpane.plist");
        FakeProbe {
            secrets: Secrets {
                github_token: None,
                slack_user_token: None,
                slack_user_id: None,
            },
            current_binary_path: Some(PathBuf::from("/Users/alice/.local/bin/signalpane")),
            launch_agent_status: Ok(LaunchAgentStatus {
                label: launch_agent::LABEL,
                loaded: true,
                daemon_ipc: DaemonIpcStatus::Responsive,
                socket_path: paths.socket_file.clone(),
                log_path: paths.daemon_log.clone(),
                plist_path,
                binary_path: PathBuf::from("/Users/alice/.local/bin/signalpane"),
                warnings: Vec::new(),
            }),
            daemon_ipc: DaemonIpcStatus::Responsive,
        }
    }

    fn all_secrets() -> Secrets {
        Secrets {
            github_token: Some("ghp-secret".to_string()),
            slack_user_token: Some("xoxp-secret".to_string()),
            slack_user_id: Some("U123".to_string()),
        }
    }

    fn write_config(paths: &AppPaths, content: &str) {
        fs::create_dir_all(&paths.config_dir).expect("create config dir");
        fs::write(&paths.config_file, content).expect("write config");
    }

    fn assert_line(output: &str, expected: &str) {
        assert!(
            output.lines().any(|line| line == expected),
            "expected line {expected:?} in:\n{output}"
        );
    }

    fn assert_line_contains(output: &str, expected: &str) {
        assert!(
            output.lines().any(|line| line.contains(expected)),
            "expected line containing {expected:?} in:\n{output}"
        );
    }
}
