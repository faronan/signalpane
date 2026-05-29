# AGENTS.md

## Repository Purpose

`signalpane` はローカル開発者向けの通知 hub です。MVP は Rust CLI、foreground daemon、SQLite store、Unix domain socket IPC、fixture-tested collectors、薄い Ratatui TUI で構成します。

## Architecture Boundaries

- 通知収集は daemon / collector layer に閉じ込めます。TUI は local IPC で daemon に接続し、GitHub、Slack、Notion を直接 fetch しません。
- 永続化する local data は `src/store/` 経由で SQLite に保存します。cursor、read state、delivery state、event data を store abstraction の外へ書かないでください。
- wire/data contract は `src/model.rs`、`src/collectors/mod.rs`、`src/ipc.rs` に置きます。collector edge 以外では ad hoc JSON より typed struct を優先します。
- source-specific API logic は `src/collectors/` に置きます。GitHub と Slack の挙動は fixture で独立に検証できる形を保ちます。

## Runtime Scope

- MVP の実行形態は `signalpane daemon --foreground`、`signalpane tui`、`signalpane status`、`signalpane sources`、`signalpane mark-read <id>`、`signalpane secrets path|check|set <key>` です。
- LaunchAgent support は user-level の `signalpane launch-agent install|start|stop|uninstall|status|restart|logs` と `~/Library/LaunchAgents/com.faronan.signalpane.plist` に限定します。
- 明示依頼なしに LaunchDaemon、root-level install、Ghostty notification、OAuth、Keychain、Notion support を追加しないでください。
- MVP work で `/Library/LaunchDaemons` や root-owned location に file を作らないでください。

## Config, State, and Secrets

- Config path: `~/.config/signalpane/config.toml`
- State、database、socket path: `~/.local/state/signalpane/`
- Log path: `~/.local/state/signalpane/logs/daemon.log`
- Secrets path: `~/.local/state/signalpane/secrets.env`
- Secrets は environment variables または `secrets.env` から読みます。優先順位は `environment variables > secrets.env > none` です。
- 許可する secret key は `SIGNALPANE_GITHUB_TOKEN`、`SIGNALPANE_SLACK_USER_TOKEN`、`SIGNALPANE_SLACK_USER_ID` のみです。
- token、cookie、Slack workspace secret、GitHub token、local personal credential を repository、fixture、config example、test、log に保存しないでください。

## Collector Rules

- GitHub collector は REST Notifications API model を使い、`Last-Modified`、`If-Modified-Since`、`X-Poll-Interval` の cursor behavior を維持します。
- GitHub MVP reasons は `mention`、`team_mention`、`review_requested` です。追加 reason や deeper issue/commit fetch は follow-up scope として扱います。
- GitHub Notifications API は classic PAT 前提です。fine-grained PAT / GitHub App token 対応をこの collector に追加しないでください。
- Slack MVP は user-token polling、explicit channel allowlist、`<@USERID>` direct mention filtering です。
- Slack MVP は full Slack notification inbox を再現しません。direct mention event では `<!subteam^...>`、`<!here>`、`<!channel>` を無視します。
- API-independent parser behavior は live request logic を変更する前に fixture でカバーしてください。

## Development Workflow

- behavior change は TDD で進めます。current code を調べ、focused failing test / fixture を追加または更新し、実装してから refactor します。
- test は API-free を default にします。live GitHub / Slack call より、fixture parser test、temp SQLite store test、IPC test を優先します。
- feature work 中に broad refactor を混ぜないでください。明示的な refactor task でない限り daemon、collector、store、IPC、TUI の分離を保ちます。
- manual edit には `apply_patch` を使います。shell heredoc で file を rewrite しないでください。

## Quality Commands

code change 後は、完了前に次を実行します。

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

docs-only change では `git diff --check -- README.md AGENTS.md` と対象文言の `rg` 確認を最低限実行し、cargo gate を未実行にする場合は理由を最終報告に書きます。

Project-local Codex policy は `.codex/rules/quality.rules` でこれらの check を許可しています。GitHub Actions は `.github/workflows/ci.yml` で同じ gate を branch push と pull request に実行します。

## Release Workflow

- `.github/workflows/release.yml` は `vX.Y.Z` に一致する stable release tag だけで実行します。
- release workflow は packaging 前に cargo quality gate を繰り返します。
- binary release target は Apple Silicon Mac の `aarch64-apple-darwin` だけです。明示依頼なしに Intel / universal macOS artifact を追加しないでください。
- release assets は `signalpane-vX.Y.Z-aarch64-apple-darwin.tar.gz` と `SHA256SUMS` です。
- GitHub Release publish 前の artifact smoke は network-free、token-free、non-destructive に保ちます。scope は artifact executability、version/help output、user-level LaunchAgent status check に限定します。
- binary install/update flow は `~/.local/bin/signalpane` を使い、config、secret、database、socket、read state、log を README に書いた path の外へ書きません。

## Documentation

- `README.md` は user-facing に保ちます。setup、runtime paths、token 要件、LaunchAgent 運用、troubleshooting、configuration examples は README に置きます。
- `AGENTS.md` は agent-facing かつ repo-specific に保ちます。broad user-level workflow preference や README の詳細手順を重複させないでください。
- 新しい public command、config key、env var、persistent schema、runtime path、release asset を追加または変更する場合は、同じ change set で `README.md` を更新します。
