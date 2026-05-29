# signalpane

`signalpane` は、GitHub notifications と Slack の個人メンションをローカルで集約する developer notification hub です。MVP は Rust CLI、foreground daemon、SQLite store、Unix domain socket IPC、fixture-tested collectors、薄い Ratatui TUI で構成されています。

## 対応範囲

- GitHub: unread notification のうち `mention`、`team_mention`、`review_requested` だけを取り込みます。
- Slack: allowlist した conversation/channel 内の `<@USERID>` direct mention だけを取り込みます。
- Slack の `<!here>`、`<!channel>`、`<!subteam^...>` user group mention、full Slack notification inbox の再現は MVP 対象外です。
- 常駐は macOS user LaunchAgent のみ対応します。`sudo`、`/Library/LaunchDaemons`、root-owned path、system-wide service は使いません。
- release binary は Apple Silicon macOS 用の `aarch64-apple-darwin` のみです。

## MVP コマンド

```sh
signalpane config init
signalpane config list
signalpane daemon --foreground
signalpane tui
signalpane status
signalpane doctor
signalpane sources
signalpane mark-read <id>
signalpane launch-agent install
signalpane launch-agent uninstall
signalpane launch-agent status
signalpane launch-agent restart
signalpane launch-agent logs --lines 100
```

## Apple Silicon macOS へインストール

Rust toolchain なしで release binary を使う場合は、`v0.1.0` を使いたい release tag に置き換えて実行します。

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

`~/.local/bin` を `PATH` に入れてください。

```sh
# zsh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc
exec zsh -l

# fish
fish_add_path ~/.local/bin
```

macOS quarantine metadata で実行が止まる場合は、インストール済み binary の属性を外します。

```sh
xattr -dr com.apple.quarantine "${HOME}/.local/bin/signalpane"
```

インストール後は、debug binary ではなく release binary が使われていることを確認します。

```sh
signalpane --version
type signalpane
signalpane launch-agent status
```

`signalpane launch-agent status` の `binary_path` が `~/.local/bin/signalpane` を指していれば、LaunchAgent も release install 先を使っています。

更新時は、新しい release tag で download、checksum、extract、`install` を繰り返します。更新で置き換わるのは `~/.local/bin/signalpane` だけです。config、secret、database、socket、read state、log は変更されません。

## Runtime locations

- Config: `~/.config/signalpane/config.toml`
- State, database, socket: `~/.local/state/signalpane/`
- Logs: `~/.local/state/signalpane/logs/daemon.log`

Secrets は環境変数からだけ読みます。

- `SIGNALPANE_GITHUB_TOKEN`
- `SIGNALPANE_SLACK_USER_TOKEN`
- `SIGNALPANE_SLACK_USER_ID`

token はこの repository、`config.toml`、plist、log に保存しないでください。

## Token 要件

### GitHub

GitHub collector は REST Notifications API を使います。この endpoint は personal access token (classic) 前提です。

- 最小 scope は `notifications` です。
- private repository の issue や commit を別 endpoint で深く読む用途まで広げる場合は `repo` scope を検討します。MVP の notification polling だけなら、まず `notifications` に留めます。
- fine-grained PAT、GitHub App user access token、GitHub App installation access token では `GET /notifications` は使えません。

### Slack

Slack collector は user token 前提です。token 形式は `xoxp-...` です。

`conversations.history` で読む conversation 種別に応じて、user token に次の scope が必要です。

- public channel: `channels:history`
- private channel: `groups:history`
- DM: `im:history`
- group DM: `mpim:history`

bot token でも `conversations.history` 自体は使えますが、読める範囲は bot が参加している conversation に限られます。個人の mention inbox として使うこの MVP では user token を使ってください。

`SIGNALPANE_SLACK_USER_ID` には自分の Slack user ID を入れます。形式は通常 `U...` です。未設定でも `auth.test` で解決しますが、明示しておくと token と user ID の切り分けがしやすくなります。user ID は token ではありませんが、runtime 設定として環境変数に置きます。

## Config 設定

default config を作ります。

```sh
signalpane config init
```

`config init` は `~/.config/signalpane/config.toml` を作成し、既存 file は上書きしません。正規化された config は次で確認できます。

```sh
signalpane config list
```

Slack は channel name ではなく conversation/channel ID を `channels` に書きます。

```toml
[github]
enabled = true
poll_interval_seconds = 60

[slack]
enabled = true
channels = ["C0123456789"]
poll_interval_seconds = 60
```

代表的な ID prefix は public channel の `C...`、private channel/group の `G...`、DM の `D...` です。Slack の conversation ID は workspace や作成時期で prefix が変わることがあるため、最終的には Slack UI や API で見える ID を使ってください。

channel ID は次の方法で確認できます。

- Slack の channel details や channel management tools で channel ID を見る。
- Slack web URL の `https://app.slack.com/client/TXXXXXXX/CXXXXXXX` の conversation 部分を確認する。
- 必要な `*:read` scope がある場合は `conversations.info` で対象 conversation を確認する。

Config 変更は daemon 起動時に読み込まれます。`signalpane daemon --foreground` を使っている場合は daemon を再起動してください。LaunchAgent を使っている場合は `signalpane launch-agent restart` を実行します。

## 初回設定順

1. `signalpane config init`
2. `~/.config/signalpane/config.toml` を編集し、Slack の allowlist channel ID を設定する
3. GitHub / Slack collector を使う場合は user launchd environment に secret を設定する
4. 初回は `signalpane launch-agent install`、既に plist がある場合や設定変更後は `signalpane launch-agent restart`
5. `signalpane launch-agent status`
6. `signalpane status`
7. `signalpane sources`
8. 必要なら `signalpane launch-agent logs --lines 100`

LaunchAgent から daemon を起動する場合、secret は plist には書きません。起動前に user launchd environment へ渡します。

```sh
launchctl setenv SIGNALPANE_GITHUB_TOKEN "<github-token>"
launchctl setenv SIGNALPANE_SLACK_USER_TOKEN "<slack-user-token>"
launchctl setenv SIGNALPANE_SLACK_USER_ID "<slack-user-id>"
```

`launchctl setenv` の値は現在の user launchd session にだけ効きます。logout や reboot では永続化されません。再ログイン後は token を再設定し、その後に `signalpane launch-agent restart` を実行してください。

## macOS user LaunchAgent

インストール済み binary の path が確定してから user LaunchAgent を登録します。

```sh
signalpane launch-agent install
signalpane launch-agent status
signalpane launch-agent restart
signalpane launch-agent logs --lines 100
signalpane launch-agent uninstall
```

LaunchAgent は `~/Library/LaunchAgents/com.faronan.signalpane.plist` だけを使い、user launchd domain で動きます。生成される plist は次の daemon command を実行します。

```sh
signalpane daemon --foreground
```

stdout/stderr は `~/.local/state/signalpane/logs/daemon.log` に出ます。config、state、database、socket、collector log は runtime locations に従います。

`signalpane launch-agent status` は launchd と daemon IPC の状態を出します。

```text
label=com.faronan.signalpane
loaded=true
daemon_ipc=responsive
socket_path=/Users/alice/.local/state/signalpane/signalpane.sock
log_path=/Users/alice/.local/state/signalpane/logs/daemon.log
plist_path=/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist
binary_path=/Users/alice/.local/bin/signalpane
```

起動成功の目安は次の状態です。

- `loaded=true`
- `daemon_ipc=responsive`
- `binary_path=~/.local/bin/signalpane`
- `signalpane status` が `unread=... total=...` を返す
- `signalpane sources` に GitHub / Slack の `cursor_key`、`poll_after`、`last_success`、`last_error`、`failures` が出る

`binary_path` は plist に登録された binary path から読みます。現在実行している `signalpane` と違う場合は `warning=` が出ます。`signalpane launch-agent install` を再実行すると、現在の binary path で plist を書き直します。

`signalpane launch-agent restart` は既存 plist を使い、loaded の場合は `launchctl bootout` してから `launchctl bootstrap` します。plist は書き換えません。

`loaded=false` かつ `daemon_ipc=responsive` の場合は、foreground の `signalpane daemon --foreground` が socket を掴んでいます。その daemon を止めてから `signalpane launch-agent install` または `signalpane launch-agent restart` を実行してください。

## Diagnostics

`signalpane doctor` は config、secret の presence、LaunchAgent、daemon IPC、runtime path、registered binary path と current binary path の差分を key=value で出します。token 実値は出しません。

```text
overall_status=ok
config_exists=true
config_parse=ok
github_enabled=true
github_token_present=true
github_status=ok
slack_enabled=true
slack_user_token_present=true
slack_user_id_present=false
slack_status=warning
launch_agent_status=ok
launch_agent_loaded=true
daemon_ipc=responsive
socket_path=/Users/alice/.local/state/signalpane/signalpane.sock
socket_exists=true
log_path=/Users/alice/.local/state/signalpane/logs/daemon.log
log_exists=true
db_path=/Users/alice/.local/state/signalpane/signalpane.sqlite3
db_exists=true
plist_path=/Users/alice/Library/LaunchAgents/com.faronan.signalpane.plist
plist_exists=true
registered_binary_path=/Users/alice/.local/bin/signalpane
current_binary_path=/Users/alice/.local/bin/signalpane
binary_path_match=true
warning=slack is enabled but SIGNALPANE_SLACK_USER_ID is not present; runtime will resolve it with auth.test
```

`overall_status=error` の場合、`signalpane doctor` は診断結果を stdout に出した後で exit code `1` を返します。Slack の `SIGNALPANE_SLACK_USER_ID` 未設定は、live API call なしでは確定できないため warning に留めます。

`signalpane status` の先頭行は安定しています。

```text
unread=0 total=0
```

続けて daemon log path と source/cursor ごとの状態を出します。`signalpane sources` も source、label、enabled、unread、cursor に加えて次を出します。

- `cursor_key`: 例 `notifications`、`channel:C0123456789`
- `poll_after`: 次に poll できる時刻
- `last_success`: cursor の最終成功時刻
- `last_error_at`: 最終 error 時刻、なければ `-`
- `last_error`: 最終 error、なければ `-`
- `failures`: 連続失敗数

collector error は SQLite cursor metadata と `~/.local/state/signalpane/logs/daemon.log` に記録します。GitHub や Slack の一部 channel が失敗しても、他 collector は止めません。

API request timeout は 10 秒固定です。成功時は API 側の retry hint を優先します。GitHub は `X-Poll-Interval`、Slack は `Retry-After` を見ます。hint がなければ config の `poll_interval_seconds` を使います。失敗時は 60 秒から始まり最大 15 分まで exponential backoff します。

## よくある失敗

| 症状                                        | 見る場所                                                                                                                                             | 対処                                                                                                                       |
| ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `daemon_ipc=unreachable`                    | `signalpane launch-agent status`, `signalpane status`, `signalpane launch-agent logs --lines 100`, `launchctl print gui/$UID/com.faronan.signalpane` | 起動直後なら少し待つ。log の collector error と plist の `binary_path` を確認する。                                        |
| GitHub `401 Unauthorized`                   | `signalpane sources`, daemon log                                                                                                                     | `SIGNALPANE_GITHUB_TOKEN` の値、classic PAT かどうか、`notifications` または `repo` scope を確認する。                     |
| Slack `missing_scope`                       | `signalpane sources`, daemon log                                                                                                                     | conversation 種別に応じて `channels:history` / `groups:history` / `im:history` / `mpim:history` を user token に追加する。 |
| Slack `channel_not_found`                   | `signalpane sources`, daemon log                                                                                                                     | `config.toml` が channel name ではなく ID を使っているか、ID の workspace が token と一致しているか確認する。              |
| Slack `not_in_channel`                      | `signalpane sources`, daemon log                                                                                                                     | user token の user が対象 private channel / DM / group DM を読めるか、所属・可視性・scope を確認する。                     |
| `loaded=false` かつ `daemon_ipc=responsive` | `signalpane launch-agent status`                                                                                                                     | foreground daemon が socket を掴んでいる。foreground daemon を止めてから LaunchAgent を install/restart する。             |

## セキュリティ注意

- README、config example、fixture、plist、log、repository に token 実値を書かないでください。
- `SIGNALPANE_GITHUB_TOKEN` と `SIGNALPANE_SLACK_USER_TOKEN` は secret です。
- `SIGNALPANE_SLACK_USER_ID` は token ではありませんが、runtime の識別情報として環境変数に置きます。
- `launchctl setenv ... "<token>"` を shell に直接入力すると shell history に残る可能性があります。誤って残した場合は history から消し、必要なら token を rotate してください。

## CI と release

GitHub Actions は branch push と pull request で次を実行します。

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

`vX.Y.Z` tag を push すると、次の asset を持つ GitHub Release を publish します。

- `signalpane-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- `SHA256SUMS`

Release binary は Apple Silicon Mac 向けです。現時点では notarized macOS app bundle ではありません。

GitHub Release を publish する前に、release workflow は macOS Apple Silicon runner で tarball smoke を実行します。`SHA256SUMS` を検証し、artifact を展開し、binary が executable であること、`signalpane --help`、`signalpane --version`、isolated config/state での `signalpane launch-agent status` を確認します。

この smoke test は distribution binary の最小実行性だけを確認します。local install path、shell `PATH`、macOS quarantine、daemon IPC、live GitHub / Slack collectors、tokens、network access、Homebrew、self-update、SLSA、SBOM は保証しません。
