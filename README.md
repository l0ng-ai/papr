<div align="center">

<img src="docs/logo.svg" alt="Papr" width="96" height="96" />

# Papr

A fast, native RSS reader for the desktop — with an agent-facing CLI.

<img src="docs/screenshot.webp" alt="Papr" width="820" />

</div>

## Features

- **Feeds & folders** — subscribe, organize, and import/export OPML.
- **Smart views** — All, Unread, Starred, and Read Later, with live counts.
- **Tags & rules** — color-coded tags and rules that tag new articles automatically.
- **Full-text** — fetch and clean the complete article when a feed ships only a summary.
- **AI** — summaries, ask-the-article Q&A, and digests. Bring your own API key.
- **Audio** — a built-in player that follows you from article to article.
- **FreshRSS sync** — keep read state in step with a FreshRSS server.
- **Local-first** — everything in a local SQLite database. No account, no cloud.
- **Localized** — English, Japanese, and Simplified Chinese.

## Installation

### macOS

Install the desktop app with [Homebrew](https://brew.sh):

```sh
brew install --cask l0ng-ai/papr/papr
```

### All platforms

Download the installer for your platform from the [latest release](https://github.com/l0ng-ai/papr/releases/latest).

## The `papr` CLI — for agents

`papr` is a token-efficient, command-line companion for **autonomous agents** to
drive Papr over the shell. It reads the same local database as the app (via the
shared `papr-core` crate), so an agent can work your feeds with no GUI:

- **Read** — `feeds`, `list`, `read`, full-text `search`
- **Triage** — `mark` read/star/later, `extract` full text, `refresh`
- **Manage** — subscriptions, folders, tags, rules, highlights, OPML
- **Sync** — FreshRSS / Miniflux
- **Agent-native output** — [TOON](https://toonformat.dev) on stdout (~40% fewer
  tokens than JSON), minimal schemas, definitive counts, structured exit codes

```sh
brew install l0ng-ai/papr/papr-cli
```

### Plug it into your agent with the `papr-rss` skill

The bundled **[`papr-rss` skill](skills/papr-rss/SKILL.md)** is the easiest way to
hand a skill-aware agent (Claude Code, Codex, OpenCode…) the keys to your feeds.
Install it in one line with [`skills`](https://github.com/vercel-labs/skills):

```sh
npx skills add https://github.com/l0ng-ai/papr/tree/main/skills/papr-rss
```

See **[docs/cli.md](docs/cli.md)** for the full command reference and install
options.
