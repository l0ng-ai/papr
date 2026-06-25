---
name: papr-rss
description: >-
  Read, search and triage the user's Papr RSS subscriptions from the shell via
  the `papr` CLI. Use when the user wants to catch up on their feeds, find or
  summarize articles they've subscribed to, check what's unread, star/save
  articles, subscribe to a new feed, or pull new posts. Triggers on: "what's in
  my feeds", "any unread RSS", "summarize this feed", "search my subscriptions
  for X", "mark these read", "subscribe to <url>", "refresh my feeds".
---

# Papr RSS (AXI CLI)

`papr` is a token-efficient, agent-facing CLI over the user's local Papr RSS
database. It emits [TOON](https://toonformat.dev) on stdout (≈40% cheaper than
JSON, via the official `toon-format` encoder), keeps diagnostics on stderr, and
returns structured errors with exit codes (0 success/no-op, 1 runtime, 2 usage). Reads are token-minimal by default;
long article bodies are truncated with a `--full` escape hatch.

Run `papr` with no arguments first — it prints the unread dashboard plus the
most useful next commands, so you can orient without reading a manual.

## Core flow

```sh
papr                        # home: unread/starred counts + recent unread + next steps
papr feeds                  # subscriptions grouped by folder, with unread counts
papr list --feed <id>       # articles in a feed (defaults to unread; --all for read too)
papr list --starred         # smart views: --starred / --later / --tag <id> / --folder <id>
papr read <id> [<id>...]    # plain-text body, truncated; pass several ids to batch
papr read --feed <id> --unread --limit 5   # read a feed's latest unread in one call
papr read <id> --full       # the complete body when truncation hid something
papr search "<query>"       # FTS5 full-text search across every article
```

## Triage & subscriptions

```sh
papr mark read <id> [<id>...]      # state: read|unread|star|unstar|later|unlater (idempotent)
papr mark-all --feed <id>          # mark a whole view read
papr subscribe <url>               # auto-discovers the feed, inserts it, fetches it
papr refresh [--feed <id>]         # fetch new articles over the network (RSS + newsletters)
papr extract <id>                  # fetch & store the cleaned full text of an article
```

## Management (mirrors the desktop app)

```sh
papr tags | papr tag add <tag_id> <article_id> | papr tag create "<name>"
papr folders | papr folder create "<name>" | papr feed move <id> --folder <id>
papr rules | papr rule create "<name>" "<keywords>" --action star
papr highlights [--article <id>] | papr highlight create <article_id> "<quote>"
papr newsletters | papr newsletter add --title .. --host .. --user .. --password ..
papr opml import <file> | papr opml export
papr settings get <key> | papr settings set <key> <value>
papr stats
```

## AI & sync (use the user's configured provider / server)

```sh
papr summarize <id> [--save]       # TL;DR + bullets for one article
papr ask "<question>"              # answer from the user's feeds (RAG over FTS5), cited
papr digest [--limit 30]           # themed briefing of recent articles
papr translate <id> --lang "English"
papr sync status | papr sync run   # reconcile read/starred + subscriptions with FreshRSS/Miniflux
```

AI commands read the provider/key from settings; if none is configured they
fail with exit 1 and tell you which `papr settings set` to run.

Destructive verbs (`unsubscribe`, `*delete`, `admin cleanup|vacuum|reset`) require
`--yes`; without it they fail with exit 2 and tell you the exact command to re-run.

## Notes

- Every command takes `--db <path>` (or the `PAPR_DB` env var) if the database
  is not in the desktop app's default location.
- Output is data, not prose. Each list states a definitive total
  (`count: N of M unread`) so you never need to paginate just to learn the size.
- If the answer is "nothing", the command says so explicitly — a zero is an
  answer, not a reason to retry with different flags.
- Prefer the ambient SessionStart hook (`papr setup`) so the unread dashboard is
  already in context at the start of a conversation; this skill is the
  lower-overhead alternative that loads only when a feed task comes up.
