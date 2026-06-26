# The `papr` CLI

`papr` is a token-efficient, command-line companion to the [Papr](../README.md)
desktop RSS reader, built for **autonomous agents** to drive over shell execution.
It reads the same local SQLite database the app maintains — through the shared
`papr-core` crate, so its queries, migrations and feed parsing can never drift
from the desktop — letting an agent catch you up on your feeds, search your
subscriptions, triage unread items, and pull new posts, all without a GUI.

## Agent-facing ergonomics

- **[TOON](https://toonformat.dev) on stdout** — ~40% fewer tokens than JSON,
  encoded by the official [`toon-format`](https://crates.io/crates/toon-format)
  crate, so quoting and tabular layout follow the spec exactly.
- **Minimal schemas** — lists return an id, a title and a status, not 12 columns;
  `papr list --fields author,url,snippet,type,feed_id,published` adds more on demand.
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
unread: 206
starred: 17
later: 0
feeds: 15
articles[10]{id,feed,title,flags,date}:
  3664,V2EX,[Java] 使用 kkRepo 搭建 Maven 私服,unread.star,"2026-06-25"
  ...
help[4]: Run `papr read <id>` to read an article's full text,...
```

## Install

**Homebrew (macOS / Linux):**

```sh
brew install l0ng-ai/papr/papr-cli
```

**Prebuilt binary** — download `papr-<target>.tar.gz` (`.zip` on Windows) from the
[latest release](https://github.com/l0ng-ai/papr/releases/latest), unpack it, and
drop `papr` anywhere on your `PATH`. The macOS builds are Developer ID signed and
notarized.

**From source:**

```sh
cargo build --release -p papr-cli      # produces target/release/papr
cp target/release/papr ~/.local/bin/   # or anywhere on PATH
```

## Giving an agent access

**Recommended: the installable skill.** The bundled
[`skills/papr-rss`](../skills/papr-rss/SKILL.md) skill loads **on demand** when an
agent recognizes a feed-related task, so it costs nothing until you actually use
it. This is the right choice for almost everyone — use it in any agent that
supports the skill format.

Install it with [`skills`](https://github.com/vercel-labs/skills):

```sh
npx skills add https://github.com/l0ng-ai/papr/tree/main/skills/papr-rss
```

Or point it at the whole repo (`npx skills add l0ng-ai/papr`) and it will discover
the skill. Either way it lands in your agent's skills directory ready to load.

**Optional: an ambient SessionStart hook.** If you want the agent to be
*proactively* aware of your feeds — every conversation opening with your unread
dashboard already in context, no prompt needed — `papr setup` wires that up.

```sh
papr setup                # wires up Claude Code, Codex and OpenCode
papr setup --app claude
```

The trade-off is a **per-session token cost in every conversation**, whether or
not it's about RSS — so reach for this only when you're running a dedicated
reading/news assistant, not a general-purpose agent. (Installs are idempotent and
repair the binary path on re-run.)

## Command reference

| Area | Commands |
| --- | --- |
| Read | `papr`, `feeds`, `list`, `read`, `search` |
| Triage | `mark`, `mark-all`, `extract`, `refresh` |
| Subscriptions | `subscribe`, `unsubscribe`, `feed`, `folder`, `folders`, `opml` |
| Organise | `tags`, `tag`, `rules`, `rule`, `highlights`, `highlight` |
| Newsletters | `newsletters`, `newsletter add/remove` |
| Sync | `sync status/connect/disconnect/run` (FreshRSS / Miniflux) |
| System | `settings`, `stats`, `admin`, `setup` |

Every command supports `--help`. Point at a non-default database with `--db <path>`
or the `PAPR_DB` environment variable.

## No second AI provider

The CLI deliberately ships **no** summarize/ask/digest/translate commands: the
agent driving `papr` is already a language model, so it reads article text with
`papr read` and `papr search` and applies its own intelligence — no second AI
provider, API key, or round-trip required. (The desktop app keeps its own AI
features; only the CLI surface omits them.)
