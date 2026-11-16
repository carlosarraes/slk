# slk

Read-only Slack CLI for agents, written in Rust and based on [slackcli](https://github.com/shaharia-lab/slackcli).
It can list, read and search. It has no command that posts, edits, reacts or marks messages as read, and the HTTP client refuses any Slack method that isn't on a read-only allowlist.

## Install

```sh
curl -sSf https://raw.githubusercontent.com/carlosarraes/slk/main/install.sh | sh
```

The installer checks the binary against the release's `SHA256SUMS`. To build from source instead, run `just build` (installs to `~/.local/bin/slk`).

## Auth

```sh
SLK_LOGIN_TOKEN=xoxp-... slk auth login      # user token (search works) or xoxb- bot token
slk auth parse-curl --login < request.curl   # browser session: DevTools → Network → any /api/ call → Copy as cURL
SLK_XOXC=... SLK_XOXD=... slk auth login-browser --workspace-url https://acme.slack.com
SLK_TOKEN=xoxp-... slk conversations list    # ephemeral, nothing stored
```

Credentials are stored in `~/.config/slk/workspaces.json` (mode 0600). `slk auth list` and `slk auth set-default` manage several workspaces.

## Use

```sh
slk conversations read '#general' --limit 20
slk conversations read https://acme.slack.com/archives/C123/p1700000000123456   # opens the thread
slk search messages "deploy failed" --in '#ops' --from '@ana'
slk conversations unread --messages
slk users info ana@acme.com
```

Output is JSON when stdout isn't a terminal. Use `--fields ts,author,text` to cut tokens. `slk llm` prints the full agent guide, and `just install-skill` installs it as a Claude Code skill.
