# AGENTS.md

## Repository Purpose

`signalpane` is a local developer notification hub. The MVP is a Rust CLI
application with a foreground daemon, SQLite store, Unix domain socket IPC,
fixture-tested collectors, and a thin Ratatui TUI.

## Architecture Boundaries

- Keep notification collection in the daemon/collector layer. The TUI must attach
  to the daemon over local IPC and must not fetch GitHub, Slack, or Notion data
  directly.
- Keep persistent local data in SQLite through `src/store/`. Avoid writing cursor,
  read state, delivery state, or event data outside the store abstraction.
- Keep wire/data contracts in `src/model.rs`, `src/collectors/mod.rs`, and
  `src/ipc.rs`. Prefer typed structs over ad hoc JSON handling outside collector
  edges.
- Keep source-specific API logic in `src/collectors/`. GitHub and Slack behavior
  should remain independently testable with fixtures.

## Runtime Scope

- MVP execution is foreground only: `signalpane daemon --foreground`,
  `signalpane tui`, `signalpane status`, `signalpane sources`, and
  `signalpane mark-read <id>`.
- LaunchAgent support is limited to the user-level
  `signalpane launch-agent install|uninstall|status` commands and
  `~/Library/LaunchAgents/com.faronan.signalpane.plist`.
- Do not add LaunchDaemon, root-level install, Ghostty notification, OAuth,
  Keychain, or Notion support unless the task explicitly asks for that follow-up
  scope.
- Do not create files under `/Library/LaunchDaemons` or any root-owned location
  for MVP work.

## Config, State, and Secrets

- Config path: `~/.config/signalpane/config.toml`.
- State, database, and socket path: `~/.local/state/signalpane/`.
- Log path: `~/.local/state/signalpane/logs/daemon.log`.
- Secrets are environment variables only:
  `SIGNALPANE_GITHUB_TOKEN`, `SIGNALPANE_SLACK_USER_TOKEN`, and
  `SIGNALPANE_SLACK_USER_ID`.
- Never store tokens, cookies, Slack workspace secrets, GitHub tokens, or local
  personal credentials in this repository, fixtures, config examples, tests, or
  logs.

## Collector Rules

- GitHub collector should use the REST Notifications API model and preserve
  cursor behavior around `Last-Modified`, `If-Modified-Since`, and
  `X-Poll-Interval`.
- GitHub MVP reasons are `mention`, `team_mention`, and `review_requested`.
- Slack MVP is user-token polling with an explicit channel allowlist and direct
  user mention filtering for `<@USERID>`.
- Slack MVP must not attempt to reproduce the full Slack notification inbox.
  Ignore `<!subteam^...>`, `<!here>`, and `<!channel>` for direct mention events.
- API-independent parser behavior should be covered by fixtures before changing
  live request logic.

## Development Workflow

- Use TDD for behavior changes: inspect current code, add or update a focused
  failing test/fixture, implement the change, then refactor.
- Keep tests API-free by default. Prefer fixture parser tests, temp SQLite store
  tests, and IPC tests over live GitHub or Slack calls.
- Avoid broad refactors during feature work. Preserve the daemon, collector,
  store, IPC, and TUI separation unless the task is explicitly a refactor.
- Use `apply_patch` for manual edits. Do not rewrite files with shell heredocs.

## Quality Commands

Run these before finishing code changes:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Project-local Codex policy in `.codex/rules/quality.rules` allows these checks
without extra approval in this repository.

GitHub Actions mirrors the local gate in `.github/workflows/ci.yml`. Branch
pushes and pull requests must run `cargo test`, `cargo fmt --check`, and
`cargo clippy --all-targets -- -D warnings`.

## Release Workflow

- `.github/workflows/release.yml` runs only for stable release tags matching
  `vX.Y.Z`.
- The release workflow repeats the cargo quality gate before packaging.
- Binary releases target Apple Silicon Macs only:
  `aarch64-apple-darwin`. Do not add Intel or universal macOS artifacts unless
  the task explicitly asks for that scope.
- Release assets are `signalpane-vX.Y.Z-aarch64-apple-darwin.tar.gz` and
  `SHA256SUMS`, uploaded to a published GitHub Release.
- Before publishing a GitHub Release, the workflow must smoke the generated
  tarball by extracting it and running only network-free, token-free,
  non-destructive commands from the packaged binary. Keep this smoke limited to
  artifact executability, version/help output, and user-level LaunchAgent status
  checks unless a follow-up task explicitly expands the release gate.
- The binary install/update flow uses `~/.local/bin/signalpane` and must not
  write config, secrets, database, socket, read state, or logs outside the paths
  documented in this file.

## Documentation

- Keep `README.md` user-facing and concise: setup, runtime paths, commands, and
  configuration examples.
- Keep `AGENTS.md` agent-facing and repo-specific. Do not duplicate broad
  user-level workflow preferences unless this repository needs a stricter rule.
- Document any new public command, config key, env var, or persistent schema
  change in the same change set.
