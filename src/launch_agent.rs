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
        path::Path,
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

        let launchctl = SystemLaunchctl;
        let service_target = service_target()?;
        bootout_if_loaded(&launchctl, &service_target)?;
        let domain = gui_domain()?;
        let output = launchctl.bootstrap(&domain, &plist_path)?;
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
        let launchctl = SystemLaunchctl;
        uninstall_with(plist_path, &service_target, &launchctl)
    }

    fn uninstall_with(
        plist_path: std::path::PathBuf,
        service_target: &str,
        launchctl: &impl Launchctl,
    ) -> Result<LaunchAgentStatus> {
        bootout_if_loaded(launchctl, service_target)?;

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
            loaded: is_loaded(&SystemLaunchctl)?,
        })
    }

    trait Launchctl {
        fn print(&self, service_target: &str) -> Result<Output>;
        fn bootout(&self, service_target: &str) -> Result<Output>;
        fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<Output>;
    }

    struct SystemLaunchctl;

    impl Launchctl for SystemLaunchctl {
        fn print(&self, service_target: &str) -> Result<Output> {
            run_launchctl([OsStr::new("print"), OsStr::new(service_target)])
        }

        fn bootout(&self, service_target: &str) -> Result<Output> {
            run_launchctl([OsStr::new("bootout"), OsStr::new(service_target)])
        }

        fn bootstrap(&self, domain: &str, plist_path: &Path) -> Result<Output> {
            run_launchctl([
                OsStr::new("bootstrap"),
                OsStr::new(domain),
                plist_path.as_os_str(),
            ])
        }
    }

    fn is_loaded(launchctl: &impl Launchctl) -> Result<bool> {
        let service_target = service_target()?;
        is_loaded_with(launchctl, &service_target)
    }

    fn is_loaded_with(launchctl: &impl Launchctl, service_target: &str) -> Result<bool> {
        let output = launchctl.print(service_target)?;
        print_output_loaded(output)
    }

    fn print_output_loaded(output: Output) -> Result<bool> {
        if output.status.success() {
            return Ok(true);
        }
        if is_service_not_found(&output) {
            return Ok(false);
        }
        ensure_success(output, "launchctl print").map(|_| true)
    }

    fn bootout_if_loaded(launchctl: &impl Launchctl, service_target: &str) -> Result<()> {
        if !is_loaded_with(launchctl, service_target)? {
            return Ok(());
        }
        let output = launchctl.bootout(service_target)?;
        ensure_bootout_success(output)
    }

    fn ensure_bootout_success(output: Output) -> Result<()> {
        if output.status.success() || is_service_not_found(&output) {
            return Ok(());
        }
        ensure_success(output, "launchctl bootout").map(|_| ())
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

    fn is_service_not_found(output: &Output) -> bool {
        output.status.code() == Some(113)
            && launchctl_output_message(output).contains("Could not find service")
    }

    fn ensure_success(output: Output, description: &str) -> Result<Output> {
        if output.status.success() {
            return Ok(output);
        }
        let message = launchctl_output_message(&output);
        if message.is_empty() {
            bail!("{description} failed with status {}", output.status);
        }
        bail!("{description} failed: {message}");
    }

    fn launchctl_output_message(output: &Output) -> String {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        [stderr.trim(), stdout.trim()]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(test)]
    mod tests {
        use std::{
            cell::RefCell,
            fs,
            os::unix::process::ExitStatusExt,
            process::{ExitStatus, Output},
        };

        use tempfile::tempdir;

        use super::*;

        struct FakeLaunchctl {
            print_output: RefCell<Option<Output>>,
            bootout_output: RefCell<Option<Output>>,
        }

        impl Launchctl for FakeLaunchctl {
            fn print(&self, _service_target: &str) -> Result<Output> {
                Ok(self.print_output.borrow_mut().take().expect("print output"))
            }

            fn bootout(&self, _service_target: &str) -> Result<Output> {
                Ok(self
                    .bootout_output
                    .borrow_mut()
                    .take()
                    .expect("bootout output"))
            }

            fn bootstrap(&self, _domain: &str, _plist_path: &std::path::Path) -> Result<Output> {
                panic!("bootstrap should not be called")
            }
        }

        #[test]
        fn print_not_found_output_means_service_is_unloaded() {
            let output = command_output(
                113,
                "",
                "Bad request.\nCould not find service \"com.faronan.signalpane\" in domain for user gui: 501\n",
            );

            assert!(!print_output_loaded(output).expect("classify output"));
        }

        #[test]
        fn bootout_non_not_found_failure_returns_error() {
            let output = command_output(5, "", "Input/output error\n");

            let err = ensure_bootout_success(output).expect_err("bootout should fail");

            assert!(
                err.to_string().contains("launchctl bootout failed"),
                "unexpected error: {err:#}"
            );
        }

        #[test]
        fn uninstall_does_not_remove_plist_when_bootout_fails() {
            let dir = tempdir().expect("tempdir");
            let plist_path = dir.path().join("com.faronan.signalpane.plist");
            fs::write(&plist_path, "plist").expect("write plist");
            let launchctl = FakeLaunchctl {
                print_output: RefCell::new(Some(command_output(0, "service = true\n", ""))),
                bootout_output: RefCell::new(Some(command_output(5, "", "Input/output error\n"))),
            };

            let err = uninstall_with(
                plist_path.clone(),
                "gui/501/com.faronan.signalpane",
                &launchctl,
            )
            .expect_err("uninstall should fail");

            assert!(
                err.to_string().contains("launchctl bootout failed"),
                "unexpected error: {err:#}"
            );
            assert!(plist_path.exists());
        }

        fn command_output(code: i32, stdout: &str, stderr: &str) -> Output {
            Output {
                status: ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            }
        }
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
