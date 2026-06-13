# CLAUDE.md — Papr

## Project Overview

**Papr** is a fast, native, local-first RSS reader for the desktop, built with Tauri 2 (Rust backend + React/TypeScript frontend). All data lives in a local SQLite database — no cloud account required.

- **Version**: 0.6.1
- **License**: MIT (Copyright 2026 l0ng-ai)
- **Identifier**: `com.thomas.papr`
- **Repository**: `l0ng-ai/papr`

## Tech Stack

| Layer | Technologies |
|-------|-------------|
| Frontend | React 19, TypeScript 5.8, Vite 7, Zustand 5, TanStack React Query 5, TanStack React Virtual 3, i18next, marked |
| Backend | Rust (edition 2021), Tauri 2, rusqlite (WAL mode, 4-reader pool), feed-rs, reqwest, ammonia, dom_smoothie, tokio |
| Extension | Plain JavaScript, Manifest V3 (Chrome + Firefox), zero dependencies |
| Build | pnpm 9, Tauri CLI v2, Vitest 3, Cargo, GitHub Actions (Node 22, Rust stable) |

## Project Structure

```
papr-loom/
├── src/                    # React/TypeScript frontend (Tauri webview)
│   ├── main.tsx            # Entry point
│   ├── App.tsx             # Root component: three-pane layout
│   ├── api.ts              # Typed Tauri IPC wrappers (~80 exports)
│   ├── store.ts            # Zustand UI state (localStorage-persisted)
│   ├── types.ts            # TypeScript mirrors of Rust domain models
│   ├── player.ts           # Audio player Zustand store
│   ├── translation.ts      # Translation jobs Zustand store
│   ├── toast.ts            # Toast notification system with undo
│   ├── i18n.ts             # i18next setup (zh, en, ja)
│   ├── styles.css          # Full application stylesheet
│   ├── components/         # 16 React components
│   ├── hooks/              # Custom hooks (articleActions, useDismiss, useFocusTrap, useMenuKeyboard)
│   ├── lib/                # Pure utility modules
│   └── locales/            # Translation JSON files
├── src-tauri/              # Rust backend (Tauri v2)
│   ├── Cargo.toml          # Rust dependencies
│   ├── tauri.conf.json     # Tauri configuration
│   ├── capabilities/       # Tauri permission capabilities
│   └── src/
│       ├── main.rs         # Entry: calls papr_lib::run()
│       ├── lib.rs          # App setup: DB init, scheduler, 50+ command registration
│       ├── commands.rs     # All Tauri IPC command handlers (~52KB)
│       ├── db.rs           # SQLite data layer, migrations, SQL (~143KB)
│       ├── models.rs       # Domain types (serde camelCase)
│       ├── state.rs        # Shared AppState: writer mutex, read-pool, HTTP client
│       ├── error.rs        # Unified AppError with stable codes
│       ├── ai.rs           # LLM integration (Anthropic/OpenAI)
│       ├── translate.rs    # Article translation
│       ├── sanitize.rs     # HTML sanitization (ammonia)
│       ├── extraction.rs   # Full-text extraction
│       ├── opml.rs         # OPML import/export
│       ├── sync.rs         # FreshRSS/Miniflux GReader API sync
│       ├── tray.rs         # System tray with unread count
│       ├── notify.rs       # Desktop notifications
│       └── ingestion/      # Feed ingestion subsystem
│           ├── fetch.rs        # HTTP fetching
│           ├── parse.rs        # RSS/Atom parsing
│           ├── discovery.rs    # Feed discovery + deep links
│           ├── scheduler.rs    # Background refresh
│           ├── sources.rs      # Multi-source normalization (YouTube, Reddit, Mastodon, Bluesky)
│           └── newsletter.rs   # IMAP email newsletters
├── extension/              # Browser extension ("Papr -- Feed Finder")
│   ├── manifest.json       # MV3 manifest
│   ├── popup.html/js       # Toolbar popup
│   └── src/
│       ├── detect.js       # Feed detection logic
│       ├── content.js      # Content script
│       └── background.js   # Service worker (badge)
├── docs/                   # Logo & screenshots
├── public/                 # Static assets (favicons)
├── scripts/                # Utility scripts (tray icon generator)
├── index.html              # Vite entry HTML with splash screen
├── vite.config.ts          # Vite config (port 1430, HMR 1431)
├── vitest.config.ts        # Vitest config
├── tsconfig.json           # TypeScript config
├── .impeccable.md          # Design direction / brand personality
└── package.json            # Frontend dependencies & scripts
```

## Architecture

### IPC Boundary

The frontend calls the Rust backend via Tauri's `invoke()` IPC mechanism:

- **Frontend → Backend**: `src/api.ts` (typed wrappers) → `src-tauri/src/commands.rs` (command handlers)
- **Streaming data**: AI tokens and feed refresh progress flow over Tauri `Channel` objects
- **Error protocol**: Rust `AppError` serializes to `{ code, detail }`, frontend resolves `code` to i18n translation keys

### State Management

| State Type | Tool | Storage |
|-----------|------|---------|
| Server/async data (feeds, articles, counts) | TanStack React Query | Fetched from Rust backend |
| UI preferences (theme, accent, density, view mode) | Zustand | localStorage |
| Audio player state | Zustand (player.ts) | In-memory |
| Translation jobs | Zustand (translation.ts) | In-memory |
| Persistent app data | SQLite (papr.db) | App data directory |

### Database

SQLite with WAL mode. Writer connection behind async mutex; 4 read-only connections via round-robin pool for responsive UI during background refreshes.

**Schema tables**: `folders`, `feeds`, `articles`, `enclosures`, `articles_fts` (FTS5), `settings`, `sync_queue`, `tags`, `article_tags`, `rules`, `newsletter_sources`, `highlights`

Migrations managed by `rusqlite_migration` (12+ migrations).

### Key Architectural Patterns

- **Read/write separation**: WAL mode + writer mutex + 4-reader pool
- **Streaming IPC**: AI features and refresh progress via Tauri `Channel`
- **Deep links**: `papr://subscribe?url=...` scheme with cold-start buffering
- **Undo pattern**: Destructive actions use grace-period undo toast before committing
- **Font bundling**: Variable-weight woff2 via `@fontsource-variable`
- **Multi-source ingestion**: YouTube, Reddit, Mastodon, Bluesky, podcasts, newsletters — all normalized to the same feed/article model

## Design Direction

"Editorial Quiet" aesthetic. See `.impeccable.md` for full details.

- **Surfaces**: Warm paper-toned, terracotta-clay accent
- **Typography**: Inter Tight (UI), Newsreader (reading serif), JetBrains Mono (metadata), Atkinson Hyperlegible (accessibility)
- **Themes**: Light (paper white) and dark (warm low-light paper, NOT cold black)
- **Layout**: Three-pane desktop (sidebar / article list / reader)
- **Anti-patterns**: No SaaS dashboards, neon aesthetics, or glassmorphism

## Common Commands

### Frontend

```bash
pnpm install              # Install dependencies
pnpm dev                  # Start Vite dev server (port 1430)
pnpm build                # Type-check + production build
pnpm test                 # Run Vitest
pnpm tsc                  # TypeScript type-check only
```

### Backend (Rust)

```bash
cd src-tauri
cargo check               # Fast type-check
cargo test                # Run Rust tests
cargo build               # Full debug build
```

### Full Desktop App (Dev)

```bash
pnpm tauri dev            # Start frontend + Rust backend in dev mode
pnpm tauri build          # Production desktop build
```

### Browser Extension

```bash
cd extension
# Load unpacked in Chrome: chrome://extensions → Developer mode → Load unpacked
# Or in Firefox: about:debugging → Load Temporary Add-on
pnpm test                 # Run extension unit tests (Vitest)
```

## CI/CD

- **CI** (`.github/workflows/ci.yml`): Runs on push/PR to `main`. Two jobs: frontend (tsc + vite build + vitest) and Rust (cargo check + cargo test).
- **Release** (`.github/workflows/release.yml`): Triggered by `v*` tags. Builds for macOS (aarch64 + x86_64), Ubuntu, Windows. Tauri-action with updater signing, macOS code signing and notarization.
- **Claude Code** (`.github/workflows/claude.yml`): Responds to `@claude` mentions in issues/PRs.
- **Claude Code Review** (`.github/workflows/claude-code-review.yml`): Automated PR review focusing on IPC boundary, React hooks, Rust safety, security, performance, test coverage.

## Conventions

### TypeScript / React

- Strict mode enabled (`noUnusedLocals`, `noUnusedParameters`)
- Target: ES2022, module resolution: bundler
- All Tauri IPC calls go through `src/api.ts` — never call `invoke()` directly from components
- Server state → React Query; client UI state → Zustand stores
- i18n keys for all user-facing strings (translations in `src/locales/`)
- Components in `src/components/`, pure utilities in `src/lib/`, hooks in `src/hooks/`

### Rust

- Edition 2021, release profile: LTO, codegen-units=1, opt-level="s"
- All domain types in `models.rs` with `serde(rename_all = "camelCase")`
- All Tauri commands registered in `lib.rs`, implemented in `commands.rs`
- Database queries in `db.rs` — use typed query functions, not raw SQL elsewhere
- Error handling via `AppError` (defined in `error.rs`) with stable codes
- Async runtime: tokio

### Browser Extension

- Zero external dependencies
- Pure JavaScript (no TypeScript)
- Feed detection logic in `src/detect.js` — shared between content script and tests

## Important Notes

- `db.rs` (~143KB) is the largest file — contains all SQL and schema migrations
- `commands.rs` (~52KB) is the second largest — all Tauri IPC handlers
- `styles.css` (~63KB) is a single monolithic stylesheet
- The app supports deep links (`papr://subscribe?url=...`) for browser extension integration
- Feed source types: rss, youtube, podcast, mastodon, bluesky, reddit, newsletter
- Supported AI providers: Anthropic (Claude) and OpenAI
- Internationalization: Chinese (zh), English (en), Japanese (ja)
