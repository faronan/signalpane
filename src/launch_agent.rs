use std::{
    env, fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::config::AppPaths;
#[cfg(target_os = "macos")]
use crate::ipc::IpcClient;

pub const LABEL: &str = "com.faronan.signalpane";
const PLIST_FILE_NAME: &str = "com.faronan.signalpane.plist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAgentStatus {
    pub label: &'static str,
    pub loaded: bool,
    pub daemon_ipc: DaemonIpcStatus,
    pub socket_path: PathBuf,
    pub log_path: PathBuf,
    pub plist_path: PathBuf,
    pub binary_path: PathBuf,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonIpcStatus {
    Responsive,
    Unreachable,
}

impl DaemonIpcStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Responsive => "responsive",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchAgentLogs {
    pub log_path: PathBuf,
    pub exists: bool,
    pub content: String,
}

pub fn install(paths: &AppPaths) -> Result<LaunchAgentStatus> {
    platform::install(paths)
}

pub fn uninstall(paths: &AppPaths) -> Result<LaunchAgentStatus> {
    platform::uninstall(paths)
}

pub fn status(paths: &AppPaths) -> Result<LaunchAgentStatus> {
    platform::status(paths)
}

pub fn restart(paths: &AppPaths) -> Result<LaunchAgentStatus> {
    platform::restart(paths)
}

pub fn logs(paths: &AppPaths, lines: usize) -> Result<LaunchAgentLogs> {
    read_log_tail(&paths.daemon_log, lines)
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

fn read_log_tail(log_path: &Path, lines: usize) -> Result<LaunchAgentLogs> {
    let file = match fs::File::open(log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LaunchAgentLogs {
                log_path: log_path.to_path_buf(),
                exists: false,
                content: String::new(),
            });
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", log_path.display()));
        }
    };

    let mut tail = std::collections::VecDeque::new();
    for line in BufReader::new(file).lines() {
        if lines == 0 {
            break;
        }
        if tail.len() == lines {
            tail.pop_front();
        }
        tail.push_back(line?);
    }

    let mut content = tail.into_iter().collect::<Vec<_>>().join("\n");
    if !content.is_empty() {
        content.push('\n');
    }

    Ok(LaunchAgentLogs {
        log_path: log_path.to_path_buf(),
        exists: true,
        content,
    })
}

#[cfg(any(target_os = "macos", test))]
fn build_status(
    paths: &AppPaths,
    plist_path: PathBuf,
    loaded: bool,
    current_binary_path: PathBuf,
    daemon_ipc: DaemonIpcStatus,
) -> LaunchAgentStatus {
    let binary_path = registered_binary_path_from_file(&plist_path)
        .unwrap_or_else(|| current_binary_path.clone());
    let mut warnings = Vec::new();

    if binary_path != current_binary_path {
        warnings.push(format!(
            "registered binary differs from current executable: current={}",
            current_binary_path.display()
        ));
    }
    if foreground_socket_conflict(loaded, daemon_ipc) {
        warnings.push(format!(
            "foreground daemon appears to be running at {}; stop it before launch-agent install or restart",
            paths.socket_file.display()
        ));
    }

    LaunchAgentStatus {
        label: LABEL,
        loaded,
        daemon_ipc,
        socket_path: paths.socket_file.clone(),
        log_path: paths.daemon_log.clone(),
        plist_path,
        binary_path,
        warnings,
    }
}

#[cfg(any(target_os = "macos", test))]
fn registered_binary_path_from_file(plist_path: &Path) -> Option<PathBuf> {
    let plist = fs::read_to_string(plist_path).ok()?;
    registered_binary_path_from_plist(&plist).map(PathBuf::from)
}

#[cfg(any(target_os = "macos", test))]
fn registered_binary_path_from_plist(plist: &str) -> Option<String> {
    let mut in_program_arguments = false;
    for line in plist.lines().map(str::trim) {
        if line == "<key>ProgramArguments</key>" {
            in_program_arguments = true;
            continue;
        }
        if !in_program_arguments || line == "<array>" {
            continue;
        }
        if line == "</array>" {
            return None;
        }
        return plist_string_value(line).map(|value| unescape_plist_string(&value));
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn plist_string_value(line: &str) -> Option<String> {
    let value = line.strip_prefix("<string>")?.strip_suffix("</string>")?;
    Some(value.to_string())
}

#[cfg(any(target_os = "macos", test))]
fn unescape_plist_string(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        unescaped.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            unescaped.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[..=end];
        match entity {
            "&amp;" => unescaped.push('&'),
            "&lt;" => unescaped.push('<'),
            "&gt;" => unescaped.push('>'),
            "&quot;" => unescaped.push('"'),
            "&apos;" => unescaped.push('\''),
            _ => unescaped.push_str(entity),
        }
        rest = &rest[end + 1..];
    }
    unescaped.push_str(rest);
    unescaped
}

#[cfg(target_os = "macos")]
fn probe_daemon_ipc(socket_path: &Path) -> DaemonIpcStatus {
    if IpcClient::new(socket_path.to_path_buf()).status().is_ok() {
        DaemonIpcStatus::Responsive
    } else {
        DaemonIpcStatus::Unreachable
    }
}

#[cfg(any(target_os = "macos", test))]
fn foreground_socket_conflict(loaded: bool, daemon_ipc: DaemonIpcStatus) -> bool {
    !loaded && daemon_ipc == DaemonIpcStatus::Responsive
}

#[cfg(any(target_os = "macos", test))]
fn ensure_no_foreground_socket_conflict(
    loaded: bool,
    daemon_ipc: DaemonIpcStatus,
    socket_path: &Path,
) -> Result<()> {
    if foreground_socket_conflict(loaded, daemon_ipc) {
        anyhow::bail!(
            "foreground daemon appears to be running at {}; stop it before launch-agent install or restart",
            socket_path.display()
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        ffi::OsStr,
        fs, io,
        path::{Path, PathBuf},
        process::{Command, Output},
    };

    use anyhow::{Context, Result, bail};

    use crate::config::AppPaths;

    use super::{
        LABEL, LaunchAgentStatus, build_status, ensure_no_foreground_socket_conflict,
        launch_agent_path_from_env, probe_daemon_ipc, render_plist,
    };

    pub fn install(paths: &AppPaths) -> Result<LaunchAgentStatus> {
        paths.ensure_dirs()?;
        let plist_path = launch_agent_path_from_env()?;
        let plist_dir = plist_path
            .parent()
            .context("LaunchAgent path must have a parent directory")?;
        fs::create_dir_all(plist_dir)
            .with_context(|| format!("failed to create {}", plist_dir.display()))?;

        let launchctl = SystemLaunchctl;
        let service_target = service_target()?;
        let loaded = is_loaded_with(&launchctl, &service_target)?;
        let daemon_ipc = probe_daemon_ipc(&paths.socket_file);
        ensure_no_foreground_socket_conflict(loaded, daemon_ipc, &paths.socket_file)?;

        let program_path =
            std::env::current_exe().context("failed to resolve current executable")?;
        let plist = render_plist(&program_path, paths);
        fs::write(&plist_path, plist)
            .with_context(|| format!("failed to write {}", plist_path.display()))?;

        if loaded {
            bootout_loaded(&launchctl, &service_target)?;
        }
        let domain = gui_domain()?;
        let output = launchctl.bootstrap(&domain, &plist_path)?;
        ensure_success(output, "launchctl bootstrap")?;

        Ok(build_status(
            paths,
            plist_path,
            true,
            program_path,
            probe_daemon_ipc(&paths.socket_file),
        ))
    }

    pub fn uninstall(paths: &AppPaths) -> Result<LaunchAgentStatus> {
        let plist_path = launch_agent_path_from_env()?;
        let service_target = service_target()?;
        let launchctl = SystemLaunchctl;
        let current_binary_path =
            std::env::current_exe().context("failed to resolve current executable")?;
        uninstall_with(
            paths,
            plist_path,
            &service_target,
            &launchctl,
            current_binary_path,
        )
    }

    fn uninstall_with(
        paths: &AppPaths,
        plist_path: std::path::PathBuf,
        service_target: &str,
        launchctl: &impl Launchctl,
        current_binary_path: PathBuf,
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

        Ok(build_status(
            paths,
            plist_path,
            false,
            current_binary_path,
            probe_daemon_ipc(&paths.socket_file),
        ))
    }

    pub fn status(paths: &AppPaths) -> Result<LaunchAgentStatus> {
        let plist_path = launch_agent_path_from_env()?;
        let current_binary_path =
            std::env::current_exe().context("failed to resolve current executable")?;
        Ok(build_status(
            paths,
            plist_path,
            is_loaded(&SystemLaunchctl)?,
            current_binary_path,
            probe_daemon_ipc(&paths.socket_file),
        ))
    }

    pub fn restart(paths: &AppPaths) -> Result<LaunchAgentStatus> {
        paths.ensure_dirs()?;
        let plist_path = launch_agent_path_from_env()?;
        let domain = gui_domain()?;
        let service_target = service_target()?;
        let current_binary_path =
            std::env::current_exe().context("failed to resolve current executable")?;
        restart_with(
            paths,
            plist_path,
            &domain,
            &service_target,
            &SystemLaunchctl,
            current_binary_path,
        )
    }

    fn restart_with(
        paths: &AppPaths,
        plist_path: PathBuf,
        domain: &str,
        service_target: &str,
        launchctl: &impl Launchctl,
        current_binary_path: PathBuf,
    ) -> Result<LaunchAgentStatus> {
        if !plist_path.exists() {
            bail!(
                "LaunchAgent plist does not exist at {}; run `signalpane launch-agent install` first",
                plist_path.display()
            );
        }

        let loaded = is_loaded_with(launchctl, service_target)?;
        let daemon_ipc = probe_daemon_ipc(&paths.socket_file);
        ensure_no_foreground_socket_conflict(loaded, daemon_ipc, &paths.socket_file)?;

        if loaded {
            bootout_loaded(launchctl, service_target)?;
        }
        let output = launchctl.bootstrap(domain, &plist_path)?;
        ensure_success(output, "launchctl bootstrap")?;

        Ok(build_status(
            paths,
            plist_path,
            true,
            current_binary_path,
            probe_daemon_ipc(&paths.socket_file),
        ))
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
        bootout_loaded(launchctl, service_target)
    }

    fn bootout_loaded(launchctl: &impl Launchctl, service_target: &str) -> Result<()> {
        ensure_bootout_success(launchctl.bootout(service_target)?)
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
            bootstrap_output: RefCell<Option<Output>>,
            calls: RefCell<Vec<String>>,
        }

        impl Launchctl for FakeLaunchctl {
            fn print(&self, service_target: &str) -> Result<Output> {
                self.calls
                    .borrow_mut()
                    .push(format!("print {service_target}"));
                Ok(self.print_output.borrow_mut().take().expect("print output"))
            }

            fn bootout(&self, service_target: &str) -> Result<Output> {
                self.calls
                    .borrow_mut()
                    .push(format!("bootout {service_target}"));
                Ok(self
                    .bootout_output
                    .borrow_mut()
                    .take()
                    .expect("bootout output"))
            }

            fn bootstrap(&self, domain: &str, plist_path: &std::path::Path) -> Result<Output> {
                self.calls
                    .borrow_mut()
                    .push(format!("bootstrap {domain} {}", plist_path.display()));
                Ok(self
                    .bootstrap_output
                    .borrow_mut()
                    .take()
                    .expect("bootstrap output"))
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
                bootstrap_output: RefCell::new(None),
                calls: RefCell::new(Vec::new()),
            };
            let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));

            let err = uninstall_with(
                &paths,
                plist_path.clone(),
                "gui/501/com.faronan.signalpane",
                &launchctl,
                PathBuf::from("/Users/alice/.local/bin/signalpane"),
            )
            .expect_err("uninstall should fail");

            assert!(
                err.to_string().contains("launchctl bootout failed"),
                "unexpected error: {err:#}"
            );
            assert!(plist_path.exists());
        }

        #[test]
        fn restart_bootouts_loaded_service_then_bootstraps_existing_plist() {
            let dir = tempdir().expect("tempdir");
            let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
            let plist_path = dir.path().join("com.faronan.signalpane.plist");
            fs::write(
                &plist_path,
                render_plist(Path::new("/Users/alice/.local/bin/signalpane"), &paths),
            )
            .expect("write plist");
            let launchctl = FakeLaunchctl {
                print_output: RefCell::new(Some(command_output(0, "service = true\n", ""))),
                bootout_output: RefCell::new(Some(command_output(0, "", ""))),
                bootstrap_output: RefCell::new(Some(command_output(0, "", ""))),
                calls: RefCell::new(Vec::new()),
            };

            restart_with(
                &paths,
                plist_path.clone(),
                "gui/501",
                "gui/501/com.faronan.signalpane",
                &launchctl,
                PathBuf::from("/Users/alice/.local/bin/signalpane"),
            )
            .expect("restart");

            assert_eq!(
                launchctl.calls.borrow().as_slice(),
                &[
                    "print gui/501/com.faronan.signalpane",
                    "bootout gui/501/com.faronan.signalpane",
                    &format!("bootstrap gui/501 {}", plist_path.display()),
                ]
            );
        }

        #[test]
        fn restart_requires_existing_plist() {
            let dir = tempdir().expect("tempdir");
            let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
            let launchctl = FakeLaunchctl {
                print_output: RefCell::new(None),
                bootout_output: RefCell::new(None),
                bootstrap_output: RefCell::new(None),
                calls: RefCell::new(Vec::new()),
            };

            let err = restart_with(
                &paths,
                dir.path().join("missing.plist"),
                "gui/501",
                "gui/501/com.faronan.signalpane",
                &launchctl,
                PathBuf::from("/Users/alice/.local/bin/signalpane"),
            )
            .expect_err("missing plist should fail");

            assert!(
                err.to_string()
                    .contains("run `signalpane launch-agent install` first"),
                "unexpected error: {err:#}"
            );
            assert!(launchctl.calls.borrow().is_empty());
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

    pub fn uninstall(_paths: &AppPaths) -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }

    pub fn status(_paths: &AppPaths) -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }

    pub fn restart(_paths: &AppPaths) -> Result<LaunchAgentStatus> {
        bail!("signalpane launch-agent is macOS only")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

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
    fn extracts_registered_binary_path_from_generated_plist() {
        let paths = AppPaths::from_bases(
            PathBuf::from("/tmp/cfg"),
            PathBuf::from("/tmp/state & <state>"),
        );
        let plist = render_plist(Path::new("/tmp/signalpane & <bin> \"quoted\""), &paths);

        assert_eq!(
            registered_binary_path_from_plist(&plist).as_deref(),
            Some("/tmp/signalpane & <bin> \"quoted\"")
        );
    }

    #[test]
    fn status_warns_when_registered_binary_differs_from_current_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = AppPaths::from_bases(dir.path().join("cfg"), dir.path().join("state"));
        let plist_path = dir.path().join("com.faronan.signalpane.plist");
        fs::write(
            &plist_path,
            render_plist(Path::new("/Users/alice/.local/bin/signalpane"), &paths),
        )
        .expect("write plist");

        let status = build_status(
            &paths,
            plist_path,
            true,
            PathBuf::from("/tmp/debug/signalpane"),
            DaemonIpcStatus::Unreachable,
        );

        assert_eq!(
            status.binary_path,
            PathBuf::from("/Users/alice/.local/bin/signalpane")
        );
        assert_eq!(status.warnings.len(), 1);
        assert!(status.warnings[0].contains("registered binary differs"));
    }

    #[test]
    fn status_warns_when_foreground_daemon_owns_socket_without_loaded_launch_agent() {
        let paths = AppPaths::from_bases(PathBuf::from("/tmp/cfg"), PathBuf::from("/tmp/state"));

        let status = build_status(
            &paths,
            PathBuf::from("/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist"),
            false,
            PathBuf::from("/Users/alice/.local/bin/signalpane"),
            DaemonIpcStatus::Responsive,
        );

        assert_eq!(status.daemon_ipc, DaemonIpcStatus::Responsive);
        assert_eq!(status.warnings.len(), 1);
        assert!(status.warnings[0].contains("foreground daemon appears"));
        assert!(
            ensure_no_foreground_socket_conflict(
                false,
                DaemonIpcStatus::Responsive,
                &paths.socket_file,
            )
            .is_err()
        );
        assert!(
            ensure_no_foreground_socket_conflict(
                true,
                DaemonIpcStatus::Responsive,
                &paths.socket_file
            )
            .is_ok()
        );
    }

    #[test]
    fn reads_tail_of_existing_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("daemon.log");
        fs::write(&log_path, "one\ntwo\nthree\n").expect("write log");

        let logs = read_log_tail(&log_path, 2).expect("logs");

        assert!(logs.exists);
        assert_eq!(logs.content, "two\nthree\n");
    }

    #[test]
    fn missing_log_returns_empty_non_error_result() {
        let dir = tempfile::tempdir().expect("tempdir");

        let logs = read_log_tail(&dir.path().join("missing.log"), 100).expect("logs");

        assert!(!logs.exists);
        assert!(logs.content.is_empty());
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
