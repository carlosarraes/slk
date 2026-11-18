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
slk auth whoami | list [--check]                    # --check: verify each saved token (see Tips)
slk conversations list [--types public_channel,private_channel,mpim,im] [--limit N] [--cursor C]
slk conversations read <channel|permalink> [--since 2h] [--until 1d] [--limit N] [--with-replies --reply-limit N] [--thread-ts TS] [--cursor C]
slk conversations digest <channel> [--since 1d]     # counts, who spoke, busiest threads: triage before reading
slk conversations get <channel> <ts>   |   slk conversations get <permalink>
slk conversations info <channel>  |  members <channel>
slk conversations unread [--messages --max-conversations N --limit N]  |  threads
slk mentions [--since 1d]                            # messages @-mentioning you or your user groups
slk search messages "<query>" [--exact "phrase"] [--in '#chan'] [--from '@user'] [--since 3d] [--until 1d] [--limit N] [--page P]
slk search channels <q>  |  slk search people <q>
slk files list [--in '#chan'] [--since 7d] [--user @x] [--types snippets,spaces]  |  slk files get <id|url>
slk canvas read <id|url>                             # Markdown
slk list read <id|url>                               # Slack List rows keyed by column
slk users info <user>  |  slk users list [--status active|deactivated|all]
```
Times: `30m`, `2h`, `7d`, `2w` or `YYYY-MM-DD`.
Global: `-w/--workspace <profile>`, `--fields a,b.c` (trims items; implies JSON), `--json`, `--text`, `-v`.

## Shapes
Messages are chronological (oldest first) and already readable: `text` has `<@U…>` mentions replaced with names.
`{ts, user, author, text, thread_ts?, reply_count?, reactions?[{name,count}], edited?, files?, attachments?, replies?}`.
A reaction like `white_check_mark` on a question usually means it was answered. Use `ts` (and `thread_ts`) to cite or open threads.
Paging: when `next_cursor` is non-null, repeat the command with `--cursor <next_cursor>`.

## Tips
- Opening a thread: `slk conversations read <permalink>` or `read <chan> --thread-ts <ts>`.
- Search needs a user token (xoxp) or browser session; bot tokens get exit 3.
- Search operators: `in:#chan from:@user before:2026-01-01 after:2025-12-01 has:link is:thread`.
- Keep output small: `--limit 20 --fields ts,author,text`.
- `slk auth list --check` calls `auth.test` once per stored workspace and adds `valid` (bool) and `status` (`ok|expired|revoked|invalid|network|error`, plus `error` text when not ok) to each row. It prints every row and always exits 0, so read `valid`/`status` per row rather than the exit code; without `--check` it stays offline.
