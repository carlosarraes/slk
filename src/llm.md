# slk — read-only Slack CLI for agents

slk can only read. It cannot post, edit, react or mark messages as read.
Output is JSON whenever stdout is not a terminal, so pipe it or pass `--json`.
Errors go to stderr as `{"error":{"code","message","retryable","details"}}`.

## Exit codes
0 ok · 2 invalid input · 3 auth/permission · 4 not found · 5 ambiguous (see details.candidates)
6 rate limited · 7 network · 8 Slack/other · 9 config/io

## Targets
- Channel: ID (`C…`, `G…`, `D…`), `#name`, `name`, `@user` (your DM with them), or a message permalink.
- User: ID (`U…`), `@handle`, display/real name, or email.
- Timestamps: `1700000000.123456` or permalink form `p1700000000123456`.

## Commands
```
slk auth whoami | list [--check]
slk conversations list [--types public_channel,private_channel,mpim,im] [--limit N] [--cursor C]
slk conversations read <channel|permalink> [--thread-ts TS] [--limit N] [--oldest TS] [--latest TS] [--cursor C]
slk conversations get <channel> <ts>   |   slk conversations get <permalink>
slk conversations info <channel>
slk conversations members <channel> [--limit N]
slk conversations unread [--messages --max-conversations N --limit N] [--types channels,dms,groups]
slk conversations threads            # unread thread replies (browser session only)
slk search messages "<query>" [--in '#chan'] [--from '@user'] [--limit N] [--page P] [--sort score|timestamp]
slk search channels <q>  |  slk search people <q>
slk users info <user>  |  slk users list [--status active|deactivated|all]
```
Global: `-w/--workspace <profile>`, `--fields a,b.c` (trim each listed item), `--json`, `--text`, `-v`.

## Shapes
Messages are chronological (oldest first) and already readable: `text` has `<@U…>` mentions replaced with names.
`{ts, user, author, text, …}`. Use `ts` (and `thread_ts`) to cite or open threads.
Paging: when `next_cursor` is non-null, repeat the command with `--cursor <next_cursor>`.

## Tips
- Opening a thread: `slk conversations read <permalink>` or `read <chan> --thread-ts <ts>`.
- Search needs a user token (xoxp) or browser session; bot tokens get exit 3.
- Search operators: `in:#chan from:@user before:2026-01-01 after:2025-12-01 has:link is:thread`.
- Keep output small: `--limit 20 --fields ts,author,text`.
