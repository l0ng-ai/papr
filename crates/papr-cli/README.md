# papr — an Agent eXperience Interface for your RSS

`papr` is a command-line companion to the [Papr](../../README.md) desktop RSS
reader, built for **autonomous agents** to drive over shell execution. It reads
the same local SQLite database the app maintains, so an agent can catch you up on
your feeds, search your subscriptions, triage unread items, and pull new posts —
all without a GUI.

It follows the [AXI](https://agentskills.io) ergonomics standard:

- **[TOON](https://toonformat.dev) on stdout** — ~40% fewer tokens than JSON.
- **Minimal schemas** — lists return an id, a title and a status, not 12 columns.
- **Truncated long text** — article bodies preview by default; `--full` for all.
- **Pre-computed aggregates** — every list states a definitive `count: N of M`.
- **Definitive empty states** — a zero is stated, never an ambiguous blank.
- **Idempotent mutations** — re-marking something already read is a no-op (exit 0).
- **Structured errors on stdout** + exit codes (`0` ok/no-op, `1` runtime, `2` usage).
- **Diagnostics on stderr** — progress never pollutes the data stream.

Run it with no arguments to see live state and the next useful commands:

```sh
$ papr
bin: ~/.local/bin/papr
description: Read, search and triage your Papr RSS feeds from the shell.
db: ~/Library/Application Support/com.thomas.papr/papr.db
unread: 206 · starred: 17 · later: 0
feeds: 15
unread[10]{id,feed,title,flags,date}:
  3664,V2EX,[Java] 使用 kkRepo 搭建 Maven 私服,unread.star,2026-06-25
  ...
help[4]:
  Run `papr read <id>` to read an article's full text
  Run `papr list --feed <id>` to list one feed's articles
  Run `papr search "<query>"` to search every article
  Run `papr refresh` to fetch new articles
```

## Install

```sh
cargo build --release -p papr-cli      # produces target/release/papr
cp target/release/papr ~/.local/bin/   # or anywhere on PATH
```

The CLI links the app's data and ingestion code directly (the shared
`papr-core` crate), so its queries, migrations and feed parsing can never drift
from the desktop app.

## Two ways to give an agent access

You only need one; they are complementary.

### 1. Ambient SessionStart hook (recommended)

```sh
papr setup            # wires up Claude Code, Codex and OpenCode
papr setup --app claude
```

This registers a `SessionStart` integration so every agent conversation **opens
with your unread dashboard already in context** — no invocation needed. Installs
are idempotent and repair the binary path on re-run. It uses the bare name
`papr` when that's on `PATH` and resolves to this binary, otherwise the absolute
path.

### 2. Installable skill

The bundled [`skills/papr-rss`](../../skills/papr-rss/SKILL.md) skill loads on
demand when an agent recognizes a feed-related task, with no per-session token
cost. Use it in any agent that supports the skill format.

## Command reference

| Area | Commands |
| --- | --- |
| Read | `papr`, `feeds`, `list`, `read`, `search` |
| Triage | `mark`, `mark-all`, `extract`, `refresh` |
| Subscriptions | `subscribe`, `unsubscribe`, `feed`, `folder`, `folders`, `opml` |
| Organise | `tags`, `tag`, `rules`, `rule`, `highlights`, `highlight` |
| Newsletters | `newsletters`, `newsletter add/remove` |
| AI | `summarize`, `ask`, `digest`, `translate` |
| Sync | `sync status/connect/disconnect/run` (FreshRSS / Miniflux) |
| System | `settings`, `stats`, `admin`, `setup` |

Every command supports `--help`. Point at a non-default database with
`--db <path>` or the `PAPR_DB` environment variable.

## Scope

The CLI covers the desktop app's full capability surface — reads, triage,
refresh (RSS + newsletters), subscription/feed/folder/tag/rule/highlight
management, OPML, AI helpers (summaries, ask-the-article, digests, translation,
reading provider config from settings), and FreshRSS/Miniflux sync — all driven
headlessly through the shared `papr-core` crate.
