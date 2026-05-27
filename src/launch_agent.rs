use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::config::AppPaths;

pub const LABEL: &str = "com.faronan.signalpane";
const PLIST_FILE_NAME: &str = "com.faronan.signalpane.plist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAgentStatus {
    pub label: &'static str,
    pub plist_path: PathBuf,
    pub loaded: bool,
}

pub fn install(paths: &AppPaths) -> Result<LaunchAgentStatus> {
    platform::install(paths)
}

pub fn uninstall() -> Result<LaunchAgentStatus> {
    platform::uninstall()
}

pub fn status() -> Result<LaunchAgentStatus> {
    platform::status()
}

pub fn launch_agent_path_from_home(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(PLIST_FILE_NAME)
}

pub fn launch_agent_path_from_env() -> Result<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required to resolve the signalpane LaunchAgent path")?;
    Ok(launch_agent_path_from_home(&home))
}

pub fn render_plist(program_path: &Path, paths: &AppPaths) -> String {
    let program = escape_plist_string(&program_path.to_string_lossy());
    let daemon_log = escape_plist_string(&paths.daemon_log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>daemon</string>
    <string>--foreground</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{daemon_log}</string>
  <key>StandardErrorPath</key>
  <string>{daemon_log}</string>
</dict>
</plist>
"#,
        label = LABEL
    )
}

fn escape_plist_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            ch => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::OsStr,
        fs, io,
        process::{Command, Output},
    };

    use anyhow::{Context, Result, bail};

    use crate::config::AppPaths;

    use super::{LABEL, LaunchAgentStatus, launch_agent_path_from_env, render_plist};

    pub fn install(paths: &AppPaths) -> Result<LaunchAgentStatus> {
        paths.ensure_dirs()?;
        let plist_path = launch_agent_path_from_env()?;
        let plist_dir = plist_path
            .parent()
            .context("LaunchAgent path must have a parent directory")?;
        fs::create_dir_all(plist_dir)
            .with_context(|| format!("failed to create {}", plist_dir.display()))?;

        let program_path =
            std::env::current_exe().context("failed to resolve current executable")?;
        let plist = render_plist(&program_path, paths);
        fs::write(&plist_path, plist)
            .with_context(|| format!("failed to write {}", plist_path.display()))?;

        let service_target = service_target()?;
        let _ = run_launchctl([OsStr::new("bootout"), OsStr::new(service_target.as_str())]);
        let domain = gui_domain()?;
        let output = run_launchctl([
            OsStr::new("bootstrap"),
            OsStr::new(domain.as_str()),
            plist_path.as_os_str(),
        ])?;
        ensure_success(output, "launchctl bootstrap")?;

        Ok(LaunchAgentStatus {
            label: LABEL,
            plist_path,
            loaded: true,
        })
    }

    pub fn uninstall() -> Result<LaunchAgentStatus> {
        let plist_path = launch_agent_path_from_env()?;
        let service_target = service_target()?;
        let _ = run_launchctl([OsStr::new("bootout"), OsStr::new(service_target.as_str())]);

        match fs::remove_file(&plist_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to remove {}", plist_path.display()));
            }
        }

        Ok(LaunchAgentStatus {
            label: LABEL,
            plist_path,
            loaded: false,
        })
    }

    pub fn status() -> Result<LaunchAgentStatus> {
        let plist_path = launch_agent_path_from_env()?;
        Ok(LaunchAgentStatus {
            label: LABEL,
            plist_path,
            loaded: is_loaded()?,
        })
    }

    fn is_loaded() -> Result<bool> {
        let service_target = service_target()?;
        let output = run_launchctl([OsStr::new("print"), OsStr::new(service_target.as_str())])?;
        Ok(output.status.success())
    }

    fn gui_domain() -> Result<String> {
        Ok(format!("gui/{}", current_uid()?))
    }

    fn service_target() -> Result<String> {
        Ok(format!("{}/{}", gui_domain()?, LABEL))
    }

    fn current_uid() -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("failed to run id -u")?;
        ensure_success(output, "id -u")
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn run_launchctl<I, S>(args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new("launchctl")
            .args(args)
            .output()
            .context("failed to run launchctl")
    }

    fn ensure_success(output: Output, description: &str) -> Result<Output> {
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if message.is_empty() {
            bail!("{description} failed with status {}", output.status);
        }
        bail!("{description} failed: {message}");
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use anyhow::{Result, bail};

    use crate::config::AppPaths;

    use super::LaunchAgentStatus;

    pub fn install(_paths: &AppPaths) -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }

    pub fn uninstall() -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }

    pub fn status() -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::config::AppPaths;

    use super::*;

    #[test]
    fn resolves_only_the_user_launch_agent_path() {
        assert_eq!(
            launch_agent_path_from_home(Path::new("/Users/alice")),
            PathBuf::from("/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist")
        );
    }

    #[test]
    fn renders_daemon_foreground_plist_with_existing_log_path() {
        let paths = AppPaths::from_bases(PathBuf::from("/tmp/cfg"), PathBuf::from("/tmp/state"));
        let plist = render_plist(Path::new("/Users/alice/.local/bin/signalpane"), &paths);

        assert!(plist.contains("<key>Label</key>\n  <string>com.faronan.signalpane</string>"));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<string>/Users/alice/.local/bin/signalpane</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>--foreground</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n  <true/>"));
        assert!(
            plist.contains(
                "<key>StandardOutPath</key>\n  <string>/tmp/state/logs/daemon.log</string>"
            )
        );
        assert!(plist.contains(
            "<key>StandardErrorPath</key>\n  <string>/tmp/state/logs/daemon.log</string>"
        ));
    }

    #[test]
    fn escapes_xml_values_in_plist_strings() {
        let paths = AppPaths::from_bases(
            PathBuf::from("/tmp/cfg"),
            PathBuf::from("/tmp/state & <state>"),
        );
        let plist = render_plist(Path::new("/tmp/signalpane & <bin> \"quoted\""), &paths);

        assert!(plist.contains("/tmp/signalpane &amp; &lt;bin&gt; &quot;quoted&quot;"));
        assert!(plist.contains("/tmp/state &amp; &lt;state&gt;/logs/daemon.log"));
    }

    #[test]
    fn plist_does_not_target_root_launchd_locations_or_store_secrets() {
        let paths = AppPaths::from_bases(PathBuf::from("/tmp/cfg"), PathBuf::from("/tmp/state"));
        let plist = render_plist(Path::new("/Users/alice/.local/bin/signalpane"), &paths);

        assert!(!plist.contains("/Library/LaunchDaemons"));
        assert!(!plist.contains("<key>UserName</key>"));
        assert!(!plist.contains("SIGNALPANE_GITHUB_TOKEN"));
        assert!(!plist.contains("SIGNALPANE_SLACK_USER_TOKEN"));
        assert!(!plist.contains("SIGNALPANE_SLACK_USER_ID"));
    }
}
