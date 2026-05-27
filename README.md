# signalpane

Local developer notification hub for GitHub and Slack mentions.

## MVP commands

```sh
signalpane daemon --foreground
signalpane tui
signalpane status
signalpane sources
signalpane mark-read <id>
signalpane launch-agent install
signalpane launch-agent uninstall
signalpane launch-agent status
```

## Install on Apple Silicon macOS

Release binaries are built for Apple Silicon Macs, which use the
`aarch64-apple-darwin` Rust target. Intel Macs are not part of the current binary
release scope.

Install the latest release without Rust by replacing `v0.1.0` with the release
tag you want:

```sh
TAG=v0.1.0
TARGET=aarch64-apple-darwin
ASSET="signalpane-${TAG}-${TARGET}"
BASE_URL="https://github.com/faronan/signalpane/releases/download/${TAG}"

curl -fLO "${BASE_URL}/${ASSET}.tar.gz"
curl -fLO "${BASE_URL}/SHA256SUMS"
shasum -a 256 -c SHA256SUMS
tar -xzf "${ASSET}.tar.gz"

mkdir -p "${HOME}/.local/bin"
install -m 0755 signalpane "${HOME}/.local/bin/signalpane"
```

Make sure `~/.local/bin` is on your `PATH`.

```sh
# zsh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
exec zsh -l

# fish
fish_add_path ~/.local/bin
```

If macOS blocks a downloaded binary because of quarantine metadata, remove that
attribute from the installed binary:

```sh
xattr -dr com.apple.quarantine "${HOME}/.local/bin/signalpane"
```

To update, repeat the download, checksum, extract, and `install` steps with a
newer release tag. Updating only replaces `~/.local/bin/signalpane`; it does not
modify your config, secrets, database, socket, or logs.

## macOS user LaunchAgent

After installing the binary at its final path, register the foreground daemon as
a macOS user LaunchAgent:

```sh
signalpane launch-agent install
signalpane launch-agent status
signalpane launch-agent uninstall
```

The LaunchAgent is limited to
`~/Library/LaunchAgents/com.faronan.signalpane.plist` and the user launchd
domain. It does not use `sudo`, `/Library/LaunchDaemons`, root-owned paths, or
system-wide services. The generated plist runs the existing daemon command:

```sh
signalpane daemon --foreground
```

The plist writes stdout and stderr to
`~/.local/state/signalpane/logs/daemon.log`. Config, state, database, socket, and
collector logs continue to use the runtime locations documented below.

Secrets are not written to the plist. If the daemon needs GitHub or Slack
credentials when launched by launchd, provide them through the user launchd
environment before installing or restarting the LaunchAgent:

```sh
launchctl setenv SIGNALPANE_GITHUB_TOKEN "<github-token>"
launchctl setenv SIGNALPANE_SLACK_USER_TOKEN "<slack-user-token>"
launchctl setenv SIGNALPANE_SLACK_USER_ID "<slack-user-id>"
```

Values set with `launchctl setenv` are scoped to the current user launchd
session and are not persisted across logout or reboot. Set them again after
login before installing or restarting the LaunchAgent when collectors need
credentials.

## Runtime locations

- Config: `~/.config/signalpane/config.toml`
- State, database, socket: `~/.local/state/signalpane/`
- Logs: `~/.local/state/signalpane/logs/daemon.log`

Secrets are read from environment variables only:

- `SIGNALPANE_GITHUB_TOKEN`
- `SIGNALPANE_SLACK_USER_TOKEN`
- `SIGNALPANE_SLACK_USER_ID`

Do not store tokens in this repository or in `config.toml`.

## Setup config

The MVP reads config from `~/.config/signalpane/config.toml`. The daemon creates
the config directory if needed, but it does not generate a config file yet.
Create one manually after installing the binary:

```sh
mkdir -p ~/.config/signalpane
printf '%s\n' \
  '[github]' \
  'enabled = true' \
  'poll_interval_seconds = 60' \
  '' \
  '[slack]' \
  'enabled = true' \
  'channels = ["C0123456789"]' \
  'poll_interval_seconds = 60' \
  > ~/.config/signalpane/config.toml
```

Replace `C0123456789` with the Slack channel IDs that signalpane should poll.
Config changes are read when `signalpane daemon --foreground` starts, so restart
the foreground daemon after editing this file.

Future setup commands are planned, but not part of the current MVP:

```sh
signalpane config init
signalpane config set slack.channels C0123456789
signalpane config list
```

## Foreground daemon diagnostics

`signalpane status` keeps the first line stable:

```sh
unread=0 total=0
```

It also prints the daemon log path and per-source/cursor diagnostics. Sources
with stored cursors are listed once per cursor, so Slack channels expose
independent health rows. `signalpane sources` keeps the existing source, label,
enabled, unread, and cursor fields, and adds:

- `cursor_key`: cursor row being reported, for example `channel:C0123456789`.
- `poll_after`: next time the daemon should poll that source or channel.
- `last_success`: last successful collector run for the cursor.
- `last_error_at`: time of the most recent collector error, or `-`.
- `last_error`: most recent collector error, or `-`.
- `failures`: consecutive collector failures for the cursor.

Collector errors are written to the SQLite cursor metadata and to
`~/.local/state/signalpane/logs/daemon.log`. A failing source or Slack channel is
backed off independently and does not stop other collectors in the foreground
daemon.

API requests use a fixed 10 second timeout. Successful polls prefer source API
retry hints first (`X-Poll-Interval` for GitHub and `Retry-After` for Slack),
then fall back to `poll_interval_seconds` from config. Collector failures use an
exponential backoff starting at 60 seconds and capped at 15 minutes.

## Example config

```toml
[github]
enabled = true
poll_interval_seconds = 60

[slack]
enabled = true
channels = ["C0123456789"]
poll_interval_seconds = 60
```

Slack MVP uses a user token, an explicit channel allowlist, and message text
filtering for `<@USERID>` mentions. It does not attempt to reproduce the full
Slack notification inbox.

## CI and releases

GitHub Actions runs `cargo test`, `cargo fmt --check`, and
`cargo clippy --all-targets -- -D warnings` for branch pushes and pull requests.

Pushing a `vX.Y.Z` tag creates a published GitHub Release with these assets:

- `signalpane-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- `SHA256SUMS`

Release binaries are intended for Apple Silicon Macs and are not currently
notarized or packaged as a macOS app bundle.

Before publishing a GitHub Release, the release workflow smokes the generated
tarball on the macOS Apple Silicon runner. It extracts the artifact, verifies
that the included `signalpane` binary is executable, checks `signalpane --help`
and `signalpane --version`, and runs `signalpane launch-agent status` with
isolated config and state paths.

This smoke test confirms that the release artifact is minimally executable as a
distribution binary. It does not cover local installation paths, shell `PATH`
setup, macOS quarantine handling, daemon IPC, live GitHub or Slack collectors,
tokens, network access, Homebrew, self-update, SLSA, or SBOM guarantees.
