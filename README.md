# signalpane

Local developer notification hub for GitHub and Slack mentions.

## MVP commands

```sh
signalpane daemon --foreground
signalpane tui
signalpane status
signalpane sources
signalpane mark-read <id>
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

## Runtime locations

- Config: `~/.config/signalpane/config.toml`
- State, database, socket: `~/.local/state/signalpane/`
- Logs: `~/.local/state/signalpane/logs/daemon.log`

Secrets are read from environment variables only:

- `SIGNALPANE_GITHUB_TOKEN`
- `SIGNALPANE_SLACK_USER_TOKEN`
- `SIGNALPANE_SLACK_USER_ID`

Do not store tokens in this repository or in `config.toml`.

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
