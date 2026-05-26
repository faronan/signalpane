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
