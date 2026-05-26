use anyhow::{Result, bail};
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
            let status = IpcClient::new(paths.socket_file).status()?;
            println!(
                "unread={} total={}",
                status.unread_count, status.total_count
            );
            for source in status.sources {
                println!(
                    "{}\t{}\tenabled={}\tunread={}",
                    source.source, source.label, source.enabled, source.unread_count
                );
            }
            Ok(())
        }
        Command::Sources => {
            let sources = IpcClient::new(paths.socket_file).sources()?;
            for source in sources {
                println!(
                    "{}\t{}\tenabled={}\tunread={}\tcursor={}",
                    source.source,
                    source.label,
                    source.enabled,
                    source.unread_count,
                    source.last_cursor.unwrap_or_else(|| "-".to_string())
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
