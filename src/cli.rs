use std::io::Write;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};

use crate::{
    config::{AppPaths, Config, SecretKey, Secrets},
    daemon, doctor,
    ipc::IpcClient,
    launch_agent::{self, LaunchAgentStatus},
    logging, tui,
};

#[derive(Debug, Parser)]
#[command(
    name = "signalpane",
    version,
    about = "Local developer notification hub"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Daemon {
        #[arg(long)]
        foreground: bool,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Secrets {
        #[command(subcommand)]
        command: SecretsCommand,
    },
    Tui,
    Status,
    Doctor,
    Sources,
    MarkRead {
        id: i64,
    },
    Log {
        #[arg(long, default_value_t = logging::DEFAULT_LOG_LINES)]
        lines: usize,
    },
    LaunchAgent {
        #[command(subcommand)]
        command: LaunchAgentCommand,
    },
}

#[derive(Debug, Subcommand)]
enum LaunchAgentCommand {
    Install,
    Start,
    Stop,
    Uninstall,
    Status,
    Restart,
    Logs {
        #[arg(long, default_value_t = 100)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Init,
    List,
}

#[derive(Debug, Subcommand)]
enum SecretsCommand {
    Path,
    Check,
    Set {
        #[command(subcommand)]
        command: SecretsSetCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SecretsSetCommand {
    GithubToken,
    SlackUserToken,
    SlackUserId,
}

impl SecretsSetCommand {
    fn secret_key(&self) -> SecretKey {
        match self {
            Self::GithubToken => SecretKey::GithubToken,
            Self::SlackUserToken => SecretKey::SlackUserToken,
            Self::SlackUserId => SecretKey::SlackUserId,
        }
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = AppPaths::from_env()?;
    match cli.command {
        Command::Daemon { foreground } => {
            if !foreground {
                bail!("MVP only supports `signalpane daemon --foreground`");
            }
            paths.ensure_dirs()?;
            let config = Config::load(&paths.config_file)?;
            let secrets = Secrets::load(&paths)?;
            daemon::run_foreground(paths, config, secrets)
        }
        Command::Config { command } => match command {
            ConfigCommand::Init => {
                Config::init_file(&paths.config_file)?;
                println!("created={}", paths.config_file.display());
                Ok(())
            }
            ConfigCommand::List => {
                let config = Config::load(&paths.config_file)?;
                print!("{}", config.to_toml()?);
                Ok(())
            }
        },
        Command::Secrets { command } => match command {
            SecretsCommand::Path => {
                println!("{}", paths.secrets_file.display());
                Ok(())
            }
            SecretsCommand::Check => {
                print!("{}", format_secrets_check(&Secrets::load(&paths)?));
                Ok(())
            }
            SecretsCommand::Set { command } => {
                let key = command.secret_key();
                let value = rpassword::prompt_password(format!("{}: ", key.env_name()))?;
                Secrets::set(&paths, key, &value)?;
                println!("updated={}", key.env_name());
                println!("path={}", paths.secrets_file.display());
                println!("restart_required=true");
                println!(
                    "message=run `signalpane launch-agent restart` for a running daemon to pick up the change"
                );
                Ok(())
            }
        },
        Command::Tui => tui::run(paths.socket_file),
        Command::Status => {
            let status = IpcClient::new(paths.socket_file.clone()).status()?;
            println!(
                "unread={} total={}",
                status.unread_count, status.total_count
            );
            println!("daemon_log={}", paths.daemon_log.display());
            for source in status.sources {
                println!(
                    "{}\t{}\tenabled={}\tunread={}\tcursor_key={}\tpoll_after={}\tlast_success={}\tlast_error_at={}\tlast_error={}\tfailures={}",
                    source.source,
                    source.label,
                    source.enabled,
                    source.unread_count,
                    format_optional_text(source.cursor_key.as_deref()),
                    format_dt(source.poll_after.as_ref()),
                    format_dt(source.last_success_at.as_ref()),
                    format_dt(source.last_error_at.as_ref()),
                    format_optional_text(source.last_error.as_deref()),
                    source.consecutive_failures
                );
            }
            Ok(())
        }
        Command::Doctor => {
            let report = doctor::check(&paths);
            let output = report.to_key_value();
            let mut stdout = std::io::stdout();
            stdout.write_all(output.as_bytes())?;
            stdout.flush()?;
            if report.has_errors() {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Sources => {
            let sources = IpcClient::new(paths.socket_file).sources()?;
            for source in sources {
                println!(
                    "{}\t{}\tenabled={}\tunread={}\tcursor_key={}\tcursor={}\tpoll_after={}\tlast_success={}\tlast_error_at={}\tlast_error={}\tfailures={}",
                    source.source,
                    source.label,
                    source.enabled,
                    source.unread_count,
                    format_optional_text(source.cursor_key.as_deref()),
                    format_optional_text(source.last_cursor.as_deref()),
                    format_dt(source.poll_after.as_ref()),
                    format_dt(source.last_success_at.as_ref()),
                    format_dt(source.last_error_at.as_ref()),
                    format_optional_text(source.last_error.as_deref()),
                    source.consecutive_failures
                );
            }
            Ok(())
        }
        Command::MarkRead { id } => {
            let changed = IpcClient::new(paths.socket_file).mark_read(id)?;
            println!("changed={changed}");
            Ok(())
        }
        Command::Log { lines } => {
            print_log_tail(&logging::read_log_tail(&paths.daemon_log, lines)?);
            Ok(())
        }
        Command::LaunchAgent { command } => match command {
            LaunchAgentCommand::Install => {
                print_launch_agent_status(&launch_agent::install(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Start => {
                print_launch_agent_status(&launch_agent::start(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Stop => {
                print_launch_agent_status(&launch_agent::stop(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Uninstall => {
                print_launch_agent_status(&launch_agent::uninstall(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Status => {
                print_launch_agent_status(&launch_agent::status(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Restart => {
                print_launch_agent_status(&launch_agent::restart(&paths)?);
                Ok(())
            }
            LaunchAgentCommand::Logs { lines } => {
                print_launch_agent_logs(&launch_agent::logs(&paths, lines)?);
                Ok(())
            }
        },
    }
}

fn format_secrets_check(secrets: &Secrets) -> String {
    let mut output = String::new();
    for key in SecretKey::ALL {
        output.push_str(key.env_name());
        output.push('=');
        output.push_str(if secrets.value_for(key).is_some() {
            "present"
        } else {
            "missing"
        });
        output.push('\n');
    }
    output
}

fn print_launch_agent_status(status: &LaunchAgentStatus) {
    print!("{}", format_launch_agent_status(status));
}

fn format_launch_agent_status(status: &LaunchAgentStatus) -> String {
    let mut output = String::new();
    output.push_str(&format!("label={}\n", status.label));
    output.push_str(&format!("loaded={}\n", status.loaded));
    output.push_str(&format!("daemon_ipc={}\n", status.daemon_ipc.as_str()));
    output.push_str(&format!("socket_path={}\n", status.socket_path.display()));
    output.push_str(&format!("log_path={}\n", status.log_path.display()));
    output.push_str(&format!("plist_path={}\n", status.plist_path.display()));
    output.push_str(&format!("binary_path={}\n", status.binary_path.display()));
    for warning in &status.warnings {
        output.push_str(&format!("warning={}\n", sanitize_cli_value(warning)));
    }
    output
}

fn print_launch_agent_logs(logs: &launch_agent::LaunchAgentLogs) {
    print!("{}", format_log_tail(logs));
}

fn print_log_tail(logs: &logging::LogTail) {
    print!("{}", format_log_tail(logs));
}

fn format_log_tail(logs: &logging::LogTail) -> String {
    let mut output = String::new();
    output.push_str(&format!("log_path={}\n", logs.log_path.display()));
    output.push_str(&format!("log_exists={}\n", logs.exists));
    output.push_str(&logs.content);
    output
}

fn format_dt(value: Option<&DateTime<Utc>>) -> String {
    value
        .map(DateTime::to_rfc3339)
        .unwrap_or_else(|| "-".to_string())
}

fn format_optional_text(value: Option<&str>) -> String {
    value
        .map(sanitize_cli_value)
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
    use super::*;

    #[test]
    fn parses_config_init_command() {
        let cli = Cli::try_parse_from(["signalpane", "config", "init"]).expect("parse cli");

        match cli.command {
            Command::Config {
                command: ConfigCommand::Init,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_config_list_command() {
        let cli = Cli::try_parse_from(["signalpane", "config", "list"]).expect("parse cli");

        match cli.command {
            Command::Config {
                command: ConfigCommand::List,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_secrets_path_command() {
        let cli = Cli::try_parse_from(["signalpane", "secrets", "path"]).expect("parse cli");

        match cli.command {
            Command::Secrets {
                command: SecretsCommand::Path,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_secrets_check_command() {
        let cli = Cli::try_parse_from(["signalpane", "secrets", "check"]).expect("parse cli");

        match cli.command {
            Command::Secrets {
                command: SecretsCommand::Check,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_secrets_set_github_token_command() {
        let cli = Cli::try_parse_from(["signalpane", "secrets", "set", "github-token"])
            .expect("parse cli");

        match cli.command {
            Command::Secrets {
                command:
                    SecretsCommand::Set {
                        command: SecretsSetCommand::GithubToken,
                    },
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn formats_secrets_check_without_values() {
        let secrets = Secrets {
            github_token: Some("ghp_secret".to_string()),
            slack_user_token: None,
            slack_user_id: Some("U123".to_string()),
        };

        let output = format_secrets_check(&secrets);

        assert_eq!(
            output,
            concat!(
                "SIGNALPANE_GITHUB_TOKEN=present\n",
                "SIGNALPANE_SLACK_USER_TOKEN=missing\n",
                "SIGNALPANE_SLACK_USER_ID=present\n",
            )
        );
        assert!(!output.contains("ghp_secret"));
        assert!(!output.contains("U123"));
    }

    #[test]
    fn parses_launch_agent_restart_command() {
        let cli =
            Cli::try_parse_from(["signalpane", "launch-agent", "restart"]).expect("parse cli");

        match cli.command {
            Command::LaunchAgent {
                command: LaunchAgentCommand::Restart,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_launch_agent_start_command() {
        let cli = Cli::try_parse_from(["signalpane", "launch-agent", "start"]).expect("parse cli");

        match cli.command {
            Command::LaunchAgent {
                command: LaunchAgentCommand::Start,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_launch_agent_stop_command() {
        let cli = Cli::try_parse_from(["signalpane", "launch-agent", "stop"]).expect("parse cli");

        match cli.command {
            Command::LaunchAgent {
                command: LaunchAgentCommand::Stop,
            } => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_doctor_command() {
        let cli = Cli::try_parse_from(["signalpane", "doctor"]).expect("parse cli");

        match cli.command {
            Command::Doctor => {}
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_launch_agent_logs_lines_command() {
        let cli = Cli::try_parse_from(["signalpane", "launch-agent", "logs", "--lines", "42"])
            .expect("parse cli");

        match cli.command {
            Command::LaunchAgent {
                command: LaunchAgentCommand::Logs { lines },
            } => assert_eq!(lines, 42),
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_log_lines_command() {
        let cli = Cli::try_parse_from(["signalpane", "log", "--lines", "42"]).expect("parse cli");

        match cli.command {
            Command::Log { lines } => assert_eq!(lines, 42),
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn parses_log_default_lines_command() {
        let cli = Cli::try_parse_from(["signalpane", "log"]).expect("parse cli");

        match cli.command {
            Command::Log { lines } => assert_eq!(lines, 100),
            command => panic!("unexpected command: {command:?}"),
        }
    }

    #[test]
    fn formats_launch_agent_status_diagnostics() {
        let status = LaunchAgentStatus {
            label: launch_agent::LABEL,
            loaded: true,
            daemon_ipc: launch_agent::DaemonIpcStatus::Responsive,
            socket_path: "/tmp/state/signalpane.sock".into(),
            log_path: "/tmp/state/logs/daemon.log".into(),
            plist_path: "/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist".into(),
            binary_path: "/Users/alice/.local/bin/signalpane".into(),
            warnings: vec!["binary path differs\nfrom current executable".to_string()],
        };

        assert_eq!(
            format_launch_agent_status(&status),
            concat!(
                "label=com.faronan.signalpane\n",
                "loaded=true\n",
                "daemon_ipc=responsive\n",
                "socket_path=/tmp/state/signalpane.sock\n",
                "log_path=/tmp/state/logs/daemon.log\n",
                "plist_path=/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist\n",
                "binary_path=/Users/alice/.local/bin/signalpane\n",
                "warning=binary path differs from current executable\n",
            )
        );
    }

    #[test]
    fn formats_missing_launch_agent_log_without_error() {
        let logs = launch_agent::LaunchAgentLogs {
            log_path: "/tmp/state/logs/daemon.log".into(),
            exists: false,
            content: String::new(),
        };

        assert_eq!(
            format_log_tail(&logs),
            "log_path=/tmp/state/logs/daemon.log\nlog_exists=false\n"
        );
    }
}
