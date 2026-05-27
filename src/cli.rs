use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};

use crate::{
    config::{AppPaths, Config, Secrets},
    daemon,
    ipc::IpcClient,
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
    Tui,
    Status,
    Sources,
    MarkRead {
        id: i64,
    },
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
    }
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
