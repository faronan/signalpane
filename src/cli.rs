use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};

use crate::{
    config::{AppPaths, Config, Secrets},
    daemon,
    ipc::IpcClient,
    launch_agent::{self, LaunchAgentStatus},
    tui,
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
    Tui,
    Status,
    Sources,
    MarkRead {
        id: i64,
    },
    LaunchAgent {
        #[command(subcommand)]
        command: LaunchAgentCommand,
    },
}

#[derive(Debug, Subcommand)]
enum LaunchAgentCommand {
    Install,
    Uninstall,
    Status,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Init,
    List,
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
            let secrets = Secrets::from_env();
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
        Command::LaunchAgent { command } => {
            let status = match command {
                LaunchAgentCommand::Install => launch_agent::install(&paths)?,
                LaunchAgentCommand::Uninstall => launch_agent::uninstall()?,
                LaunchAgentCommand::Status => launch_agent::status()?,
            };
            print_launch_agent_status(&status);
            Ok(())
        }
    }
}

fn print_launch_agent_status(status: &LaunchAgentStatus) {
    println!("label={}", status.label);
    println!("plist={}", status.plist_path.display());
    println!("loaded={}", status.loaded);
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
}
