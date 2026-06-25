//! SQLite data layer. One file holds feeds, articles, FTS5 index and settings.
//! All SQL lives here; commands call typed functions, never raw SQL.

use crate::error::{AppError, AppResult};
use crate::models::*;
use rusqlite::functions::FunctionFlags;
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::LazyLock;

/// Append-only schema migrations. Never edit a shipped migration — add a new one.
static MIGRATIONS: LazyLock<Migrations> = LazyLock::new(|| {
    Migrations::new(vec![M::up(
        r#"
        CREATE TABLE folders (
            id        INTEGER PRIMARY KEY,
            name      TEXT NOT NULL,
            position  INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE feeds (
            id              INTEGER PRIMARY KEY,
            feed_url        TEXT NOT NULL UNIQUE,
            site_url        TEXT,
            title           TEXT NOT NULL,
            description     TEXT,
            favicon_url     TEXT,
            folder_id       INTEGER REFERENCES folders(id) ON DELETE SET NULL,
            source_type     TEXT NOT NULL DEFAULT 'rss',
            etag            TEXT,
            last_modified   TEXT,
            last_fetched_at TEXT,
            fetch_error     TEXT,
            created_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE articles (
            id            INTEGER PRIMARY KEY,
            feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
            guid          TEXT NOT NULL,
            url           TEXT,
            title         TEXT NOT NULL,
            author        TEXT,
            summary       TEXT,
            content_html  TEXT,
            extracted_html TEXT,
            body_text     TEXT NOT NULL DEFAULT '',
            image_url     TEXT,
            ai_summary    TEXT,
            published_at  TEXT,
            fetched_at    TEXT NOT NULL DEFAULT (datetime('now')),
            is_read       INTEGER NOT NULL DEFAULT 0,
            is_starred    INTEGER NOT NULL DEFAULT 0,
            read_later    INTEGER NOT NULL DEFAULT 0,
            UNIQUE(feed_id, guid)
        );

        CREATE INDEX idx_articles_feed      ON articles(feed_id);
        CREATE INDEX idx_articles_published ON articles(published_at DESC);
        CREATE INDEX idx_articles_unread    ON articles(is_read) WHERE is_read = 0;

        CREATE TABLE enclosures (
            id         INTEGER PRIMARY KEY,
            article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
            url        TEXT NOT NULL,
            mime_type  TEXT,
            length     INTEGER
        );
        CREATE INDEX idx_enclosures_article ON enclosures(article_id);

        CREATE VIRTUAL TABLE articles_fts USING fts5(
            title, body, tokenize = 'porter unicode61'
        );

        -- Keep the FTS index in sync on delete; inserts are handled in code so
        -- that read-state updates do not trigger needless re-indexing.
        CREATE TRIGGER articles_fts_ad AFTER DELETE ON articles BEGIN
            DELETE FROM articles_fts WHERE rowid = old.id;
        END;

        CREATE TABLE settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
        ),
        // v2 — placeholder. An earlier sqlite-vec semantic-search schema was
        // removed; this keeps the version count aligned for databases that
        // already applied it. Search is keyword-only (FTS5).
        M::up("-- semantic search removed; search is FTS5 keyword-only"),
        // v3 — sync support: a remote item id per article plus a small queue
        // of local read/starred changes still to push to the sync server.
        M::up(
            r#"
            ALTER TABLE articles ADD COLUMN remote_id TEXT;
            CREATE TABLE sync_queue (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                field      TEXT NOT NULL,
                value      INTEGER NOT NULL,
                PRIMARY KEY (article_id, field)
            );
            "#,
        ),
        // v4 — article tags: a flat label set plus an article↔tag join table.
        M::up(
            r#"
            CREATE TABLE tags (
                id        INTEGER PRIMARY KEY,
                name      TEXT NOT NULL UNIQUE,
                color     TEXT NOT NULL DEFAULT 'clay',
                position  INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE article_tags (
                article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                tag_id     INTEGER NOT NULL REFERENCES tags(id)     ON DELETE CASCADE,
                PRIMARY KEY (article_id, tag_id)
            );
            CREATE INDEX idx_article_tags_tag ON article_tags(tag_id);
            "#,
        ),
        // v5 — filter rules: keyword matches applied to incoming articles to
        // auto-skip noise, or auto mark-read / star them, at ingestion time.
        M::up(
            r#"
            CREATE TABLE rules (
                id         INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                enabled    INTEGER NOT NULL DEFAULT 1,
                feed_id    INTEGER REFERENCES feeds(id) ON DELETE CASCADE,
                field      TEXT NOT NULL DEFAULT 'title',
                query      TEXT NOT NULL,
                action     TEXT NOT NULL DEFAULT 'skip',
                position   INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            "#,
        ),
        // v6 — index over the effective article date the list sorts by,
        // COALESCE(published_at, fetched_at), so a dateless entry sorts by
        // when it was fetched instead of sinking below every dated article.
        // (Superseded by v12, which rebuilds this index over a `datetime()`-
        // normalised expression — see that migration for why.)
        M::up(
            "CREATE INDEX idx_articles_sort
             ON articles(COALESCE(published_at, fetched_at) DESC, id DESC);",
        ),
        // v7 — every date ordering now sorts on the effective date and uses
        // idx_articles_sort, so the original published_at-only index is dead
        // weight on each insert. Drop it.
        M::up("DROP INDEX idx_articles_published;"),
        // v8 — partial indexes mirroring idx_articles_unread for the other
        // two smart-view flags, so the Starred / Read-later sidebar counts
        // and list queries use a tiny index instead of a full table scan.
        M::up(
            "CREATE INDEX idx_articles_starred
                 ON articles(is_starred) WHERE is_starred = 1;
             CREATE INDEX idx_articles_readlater
                 ON articles(read_later) WHERE read_later = 1;",
        ),
        // v9 — index the article URL. FreshRSS reconciliation matches remote
        // items to local articles by URL (up to ~1000 lookups per sync) and
        // the dedup check tests URL existence per inserted article; both
        // full-scanned the table without this.
        M::up("CREATE INDEX idx_articles_url ON articles(url);"),
        // v10 — email-newsletter sources (feature F5). A newsletter is a
        // normal `feeds` row (source_type = 'newsletter') so it lists,
        // searches and retains like an RSS feed; this side-table holds the
        // IMAP connection details, keyed 1:1 by feed_id and cascade-deleted
        // with the feed. The app-password is stored in plaintext, the same
        // way FreshRSS sync credentials live in the `settings` table — the
        // database never leaves the user's machine.
        M::up(
            r#"
            CREATE TABLE newsletter_sources (
                feed_id   INTEGER PRIMARY KEY REFERENCES feeds(id) ON DELETE CASCADE,
                host      TEXT NOT NULL,
                port      INTEGER NOT NULL DEFAULT 993,
                username  TEXT NOT NULL,
                password  TEXT NOT NULL,
                folder    TEXT NOT NULL DEFAULT 'INBOX'
            );
            "#,
        ),
        // v11 — highlights / annotations layer (feature F7). Each highlight
        // pins a span of an article's rendered plain text. `text_offset` is
        // the character offset of the quote, and `prefix` / `suffix` carry a
        // short context window for robust re-anchoring when the rendered text
        // shifts (e.g. after full-text extraction replaces a feed snippet).
        // `note` is an optional user annotation; `color` is a palette key.
        M::up(
            r#"
            CREATE TABLE highlights (
                id          INTEGER PRIMARY KEY,
                article_id  INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
                quote       TEXT NOT NULL,
                prefix      TEXT NOT NULL DEFAULT '',
                suffix      TEXT NOT NULL DEFAULT '',
                text_offset INTEGER NOT NULL DEFAULT 0,
                color       TEXT NOT NULL DEFAULT 'yellow',
                note        TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX idx_highlights_article ON highlights(article_id);
            "#,
        ),
        // v12 — rebuild the article-sort index over the *normalised* effective
        // date. `published_at` is RFC 3339 (`2024-01-15T10:30:00+00:00`, the
        // `T`-separated form `to_rfc3339` writes) while `fetched_at` uses
        // SQLite's space-separated form (`2024-01-15 10:30:00`). The old index
        // (v6) ordered on the *raw* `COALESCE(published_at, fetched_at)`, so a
        // string `<` compared the two formats byte-for-byte — and the `T`
        // (0x54) sorts after a space (0x20), making a dated article look up to
        // a day newer than a same-instant dateless one. A list mixing both
        // kinds of rows then came out subtly out of chronological order.
        //
        // Wrapping the effective date in `datetime()` parses both formats to
        // one canonical representation. The ORDER BY clauses are wrapped to
        // match (see `list_articles` / `digest_source` / `preview_rule`); an
        // index on the raw column can't serve a `datetime()`-wrapped sort, so
        // the index expression must be wrapped identically for the planner to
        // keep using it (verified with EXPLAIN QUERY PLAN — no temp B-tree).
        M::up(
            "DROP INDEX idx_articles_sort;
             CREATE INDEX idx_articles_sort
                 ON articles(datetime(COALESCE(published_at, fetched_at)) DESC,
                             id DESC);",
        ),
        // v13 — mark feeds whose title the user has set by hand. A refresh
        // pulls the feed document's own `<title>` and `update_feed_meta`
        // `COALESCE`s it over the stored one, which silently reverted a
        // manual rename on the very next poll. This flag lets `update_feed_meta`
        // leave a user-named feed's title alone while still refreshing every
        // other piece of feed metadata.
        M::up(
            "ALTER TABLE feeds ADD COLUMN custom_title INTEGER NOT NULL DEFAULT 0;",
        ),
        // v14 — cache a translated copy of the article body. `translated_lang`
        // records the target language the cache was produced for, so a later
        // change to the translation-target setting is detected as a cache miss.
        M::up(
            "ALTER TABLE articles ADD COLUMN translated_html TEXT;
             ALTER TABLE articles ADD COLUMN translated_lang TEXT;",
        ),
        // v15 — per-feed refresh interval (minutes). NULL follows the global
        // `refresh_interval_min` setting; the 525_600 sentinel means "never".
        M::up("ALTER TABLE feeds ADD COLUMN refresh_interval_min INTEGER;"),
        // v16 — per-feed auto-translate. 0 (the default) shows the original
        // text; 1 translates an article into the configured target language the
        // moment it is opened.
        M::up(
            "ALTER TABLE feeds ADD COLUMN auto_translate INTEGER NOT NULL DEFAULT 0;",
        ),
    ])
});

/// Register Papr's custom SQL scalar functions on a freshly opened connection.
///
/// SQLite's built-in `LOWER()` only case-folds ASCII (it has no Unicode
/// awareness without the ICU extension, which the bundled build omits). Rust's
/// `str::to_lowercase()` is fully Unicode-aware. Anywhere a query needs to
/// match the case-folding the Rust code does — notably `preview_rule`, which
/// must agree with `rule_matches`'s `to_lowercase()` so the rule preview counts
/// exactly the articles live ingestion would act on — `unicode_lower` provides
/// it. SQLite scalar functions are per-connection, so this runs for every
/// connection (the writer and each pooled reader).
fn register_functions(conn: &Connection) -> AppResult<()> {
    conn.create_scalar_function(
        "unicode_lower",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            // A NULL argument folds to NULL; callers `COALESCE` beforehand, but
            // staying total here keeps the function safe to use bare.
            let value: Option<String> = ctx.get(0)?;
            Ok(value.map(|s| s.to_lowercase()))
        },
    )?;
    Ok(())
}

/// Open the writer connection: run migrations and set the write-side pragmas.
/// WAL mode is persisted in the database header, so reader connections opened
/// afterwards inherit it automatically.
pub fn open(path: &Path) -> AppResult<Connection> {
    let mut conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    MIGRATIONS.to_latest(&mut conn)?;
    register_functions(&conn)?;
    Ok(conn)
}

/// Open a read-only connection for the UI query pool. Under WAL these run
/// concurrently with the writer, so interface reads never block on a
/// background refresh. `query_only` is a safety net against an accidental
/// write on a pooled reader. Must be called after `open` has migrated.
pub fn open_reader(path: &Path) -> AppResult<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;
    register_functions(&conn)?;
    Ok(conn)
}

// ─────────────────────────── folders ───────────────────────────

pub fn list_folders(conn: &Connection) -> AppResult<Vec<Folder>> {
    let mut stmt =
        conn.prepare("SELECT id, name, position FROM folders ORDER BY position, name")?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                name: r.get(1)?,
                position: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Create a folder, or return the existing one when a folder with the same
/// name (case-insensitively) is already present.
///
/// `folders.name` carries no `UNIQUE` constraint, so without this guard two
/// folders named "Tech" — or "Tech" and "tech" — could coexist, leaving the
/// sidebar with confusing near-duplicates. Idempotent-on-name is also exactly
/// what `folder_id_by_name` (and so OPML import) wants: a feed nested under a
/// folder whose name already exists must land in *that* folder rather than a
/// freshly created twin. This mirrors `create_tag`'s case-insensitive dedup.
pub fn create_folder(conn: &Connection, name: &str) -> AppResult<i64> {
    // Trim before the dedup lookup and the insert: a name carrying surrounding
    // whitespace (a pasted OPML `<outline text=" Tech ">`, an accidental
    // trailing space) is a different string from its trimmed twin, so the
    // `COLLATE NOCASE` lookup below would miss the existing folder and spawn
    // the near-duplicate the dedup exists to prevent. Normalising here — the
    // one chokepoint every caller (UI prompt, OPML import) funnels through —
    // keeps the invariant independent of any caller-side trimming.
    let name = name.trim();
    // Reject an empty/whitespace-only name at the same chokepoint. The
    // `PromptDialog` guards the interactive path, but `import_opml` reaches
    // this through `folder_id_by_name` with no such guard: an OPML folder
    // outline labelled with only whitespace (or an empty `text`/`title`
    // attribute) would otherwise insert a blank-named folder into the sidebar,
    // indistinguishable from a glitch and impossible to tell apart from any
    // other blank folder. Mirrors the guard `rename_feed` already applies.
    if name.is_empty() {
        return Err(AppError::code("emptyFolderName"));
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM folders WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO folders(name, position) VALUES (?1, (SELECT COALESCE(MAX(position),0)+1 FROM folders))",
        params![name],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a folder, rejecting a name that collides with a *different* folder.
///
/// `create_folder` collapses same-name folders, so a rename onto an existing
/// folder's name (or a case variant of it) would otherwise recreate exactly
/// the near-duplicate that dedup prevents — leaving the two functions
/// inconsistent. Match case-insensitively and return the localisable
/// `folderNameExists` code. Renaming a folder to its own name (or a case
/// change of it) is allowed. Mirrors `rename_tag`.
pub fn rename_folder(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    // Trim so the collision check and the stored value match what `create_folder`
    // would produce — otherwise a rename to `" Tech "` slips past the clash test
    // against an existing `"Tech"` and recreates the near-duplicate.
    let name = name.trim();
    // Reject an empty/whitespace-only name, the same guard `create_folder` and
    // `rename_feed` apply: a rename to a blank string would leave the folder
    // unlabelled in the sidebar with no recovery path short of renaming it
    // again to something valid.
    if name.is_empty() {
        return Err(AppError::code("emptyFolderName"));
    }
    let clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM folders WHERE name = ?1 COLLATE NOCASE AND id != ?2",
            params![name, id],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some() {
        return Err(AppError::code("folderNameExists"));
    }
    conn.execute("UPDATE folders SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn delete_folder(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM folders WHERE id = ?1", params![id])?;
    Ok(())
}

// ─────────────────────────── feeds ───────────────────────────

pub fn find_feed_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row("SELECT id FROM feeds WHERE feed_url = ?1", params![url], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn insert_feed(
    conn: &Connection,
    feed_url: &str,
    site_url: Option<&str>,
    title: &str,
    description: Option<&str>,
    source_type: SourceType,
    folder_id: Option<i64>,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO feeds(feed_url, site_url, title, description, source_type, folder_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![feed_url, site_url, title, description, source_type.as_str(), folder_id],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Promote a feed's `source_type` once its real kind is known from the parsed
/// document — but only when it is still the generic `'rss'`.
///
/// `add_feed` classifies a feed precisely (it has the parsed feed in hand and
/// runs `parse::refine_source_type`), but `import_opml` can only call
/// `parse::detect_source_type`, which inspects the URL alone and so cannot see
/// that a feed is a podcast (audio enclosures) or a Mastodon timeline. An
/// OPML-imported podcast therefore stays mislabelled `'rss'` forever, losing
/// its source badge and podcast-specific UI. The refresh loop calls this on
/// every successful fetch to correct such feeds from their first poll onward.
///
/// The `WHERE source_type = 'rss'` guard makes this strictly a promotion: a
/// feed already classified (youtube / bluesky / podcast / mastodon / reddit /
/// newsletter) is never touched, so a re-poll cannot demote or churn the type.
pub fn refine_feed_source_type(
    conn: &Connection,
    id: i64,
    source_type: SourceType,
) -> AppResult<()> {
    if source_type == SourceType::Rss {
        return Ok(());
    }
    conn.execute(
        "UPDATE feeds SET source_type = ?2
         WHERE id = ?1 AND source_type = 'rss'",
        params![id, source_type.as_str()],
    )?;
    Ok(())
}

pub fn list_feeds(conn: &Connection) -> AppResult<Vec<Feed>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.feed_url, f.site_url, f.title, f.description, f.favicon_url,
                f.folder_id, f.source_type, f.last_fetched_at, f.fetch_error,
                (SELECT COUNT(*) FROM articles a WHERE a.feed_id = f.id AND a.is_read = 0),
                f.refresh_interval_min, f.auto_translate
         FROM feeds f ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Feed {
                id: r.get(0)?,
                feed_url: r.get(1)?,
                site_url: r.get(2)?,
                title: r.get(3)?,
                description: r.get(4)?,
                favicon_url: r.get(5)?,
                folder_id: r.get(6)?,
                source_type: r.get(7)?,
                last_fetched_at: r.get(8)?,
                fetch_error: r.get(9)?,
                unread_count: r.get(10)?,
                refresh_interval_min: r.get(11)?,
                auto_translate: r.get::<_, i64>(12)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// A feed the scheduler should fetch: `(id, feed_url, etag, last_modified)` —
/// the last two are the stored revalidators for a conditional GET.
pub type FeedToRefresh = (i64, String, Option<String>, Option<String>);

/// All feeds that need an HTTP fetch. Newsletter sources are excluded — they
/// are polled over IMAP separately (see `scheduler::poll_newsletters`); their
/// synthetic `imap://` feed_url is not an HTTP-fetchable document.
pub fn feeds_to_refresh(conn: &Connection) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter'",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The "never auto-refresh" sentinel (minutes ≈ one year). The Settings panel
/// writes it as the global interval when auto-refresh is switched off, and a
/// per-feed interval can carry it to opt one feed out of automatic refresh.
pub const REFRESH_OFF_MINUTES: i64 = 525_600;

/// Non-newsletter feeds that are *due* for a fetch: their effective interval —
/// the per-feed `refresh_interval_min`, or `global_min` when unset — has
/// elapsed since `last_fetched_at` (a never-fetched feed is always due). Feeds
/// whose effective interval is the "off" sentinel are excluded entirely. Used
/// by the background scheduler; the manual refresh still fetches every feed via
/// `feeds_to_refresh`.
pub fn feeds_due_for_refresh(
    conn: &Connection,
    global_min: i64,
) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter'
           AND COALESCE(refresh_interval_min, ?1) < ?2
           AND ( last_fetched_at IS NULL
                 OR (julianday('now') - julianday(last_fetched_at)) * 1440.0
                    >= COALESCE(refresh_interval_min, ?1) )",
    )?;
    let rows = stmt
        .query_map(params![global_min, REFRESH_OFF_MINUTES], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Non-newsletter feeds for a single feed id — the per-feed manual refresh.
/// An empty result means the id is unknown or names a newsletter source (which
/// is polled over IMAP, not fetched here).
pub fn feeds_to_refresh_for_feed(conn: &Connection, feed_id: i64) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter' AND id = ?1",
    )?;
    let rows = stmt
        .query_map(params![feed_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Non-newsletter feeds in a folder — the per-folder manual refresh.
pub fn feeds_to_refresh_in_folder(
    conn: &Connection,
    folder_id: i64,
) -> AppResult<Vec<FeedToRefresh>> {
    let mut stmt = conn.prepare(
        "SELECT id, feed_url, etag, last_modified FROM feeds
         WHERE source_type != 'newsletter' AND folder_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![folder_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Set (or clear) a feed's per-feed refresh interval. `None` reverts the feed
/// to the global interval; `Some(REFRESH_OFF_MINUTES)` opts it out entirely.
pub fn set_feed_refresh_interval(
    conn: &Connection,
    id: i64,
    minutes: Option<i64>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET refresh_interval_min = ?2 WHERE id = ?1",
        params![id, minutes],
    )?;
    Ok(())
}

/// Toggle a feed's per-feed auto-translate flag. When on, opening an article
/// from this feed starts a translation into the configured target language.
pub fn set_feed_auto_translate(conn: &Connection, id: i64, enabled: bool) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET auto_translate = ?2 WHERE id = ?1",
        params![id, enabled as i64],
    )?;
    Ok(())
}

/// Refresh a feed's metadata from its parsed document. A `None` *or empty*
/// field leaves the stored value untouched. The feed-supplied `title` is
/// applied only when the user has *not* renamed the feed by hand
/// (`custom_title = 0`); otherwise `update_feed_meta` would revert a manual
/// rename on the next poll.
///
/// Empty strings are treated exactly like `None` (`NULLIF(?, '')`): `feed-rs`
/// parses a `<title></title>` element as `Some("")`, and the scheduler's
/// refresh path passes the parsed title straight through. Without the
/// `NULLIF` guard, a feed that momentarily serves an empty `<title>` would
/// overwrite a perfectly good feed name with a blank string in the sidebar —
/// `COALESCE` only skips a SQL `NULL`, not an empty string. `add_feed`
/// already filters empty titles on the subscribe path; this makes the
/// periodic-refresh path just as safe, and applies the same protection to
/// the other metadata columns.
pub fn update_feed_meta(
    conn: &Connection,
    id: i64,
    title: Option<&str>,
    site_url: Option<&str>,
    description: Option<&str>,
    favicon_url: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET
            title       = CASE WHEN custom_title = 1 THEN title
                               ELSE COALESCE(NULLIF(?2, ''), title) END,
            site_url    = COALESCE(NULLIF(?3, ''), site_url),
            description = COALESCE(NULLIF(?4, ''), description),
            favicon_url = COALESCE(NULLIF(?5, ''), favicon_url)
         WHERE id = ?1",
        params![id, title, site_url, description, favicon_url],
    )?;
    Ok(())
}

pub fn set_feed_fetch_state(
    conn: &Connection,
    id: i64,
    etag: Option<&str>,
    last_modified: Option<&str>,
    error: Option<&str>,
) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET etag = ?2, last_modified = ?3, fetch_error = ?4,
                          last_fetched_at = datetime('now')
         WHERE id = ?1",
        params![id, etag, last_modified, error],
    )?;
    Ok(())
}

/// Record a successful fetch that produced no changes (304 Not Modified).
pub fn touch_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = NULL WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

/// A single feed's `last_fetched_at` timestamp, if it has ever been fetched.
pub fn feed_last_fetched(conn: &Connection, id: i64) -> AppResult<Option<String>> {
    Ok(conn.query_row(
        "SELECT last_fetched_at FROM feeds WHERE id = ?1",
        params![id],
        |r| r.get::<_, Option<String>>(0),
    )?)
}

/// Record a failed fetch, keeping the previous content untouched.
pub fn set_feed_error(conn: &Connection, id: i64, error: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE feeds SET last_fetched_at = datetime('now'), fetch_error = ?2 WHERE id = ?1",
        params![id, error],
    )?;
    Ok(())
}

pub fn delete_feed(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feeds WHERE id = ?1", params![id])?;
    Ok(())
}

/// Feeds for OPML export as `(title, feed_url, folder)` tuples. Newsletter
/// sources are excluded: OPML is an RSS-subscription interchange format, and a
/// newsletter's `feed_url` is a synthetic `imap://user@host:port/folder`
/// string — exporting it would emit an `<outline xmlUrl="imap://…">` that any
/// reader (Papr's own `import_opml` included) would treat as an RSS feed and
/// then fail to HTTP-fetch forever, with the IMAP credentials not even carried.
pub fn feeds_for_export(conn: &Connection) -> AppResult<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT f.title, f.feed_url, fo.name
         FROM feeds f LEFT JOIN folders fo ON fo.id = f.folder_id
         WHERE f.source_type != 'newsletter'
         ORDER BY fo.name, f.title",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Feed URLs the sync layer should mirror onto the server. Excludes
/// `newsletter` sources (those have no upstream feed URL the server could
/// subscribe to), matching the OPML-export filter.
pub fn feed_urls_for_sync(conn: &Connection) -> AppResult<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT feed_url FROM feeds WHERE source_type != 'newsletter' AND feed_url <> ''",
    )?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Find a folder by name, creating it if absent. Used during OPML import.
/// Resolve a folder name to its id, creating the folder when absent. Used by
/// OPML import to attach imported feeds to their folders. `create_folder` is
/// itself case-insensitively idempotent, so an OPML folder whose name matches
/// an existing folder (in any case) reuses that folder instead of spawning a
/// near-duplicate.
pub fn folder_id_by_name(conn: &Connection, name: &str) -> AppResult<i64> {
    create_folder(conn, name)
}

pub fn move_feed(conn: &Connection, id: i64, folder_id: Option<i64>) -> AppResult<()> {
    conn.execute("UPDATE feeds SET folder_id = ?2 WHERE id = ?1", params![id, folder_id])?;
    Ok(())
}

/// The folder a feed currently sits in, if any. Used by sync to tell an
/// already-filed feed (leave it) from an unfiled one (adopt the server folder).
pub fn feed_folder_id(conn: &Connection, id: i64) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT folder_id FROM feeds WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Set a feed's display title to a user-chosen value. `custom_title` is also
/// raised so a later refresh's `update_feed_meta` does not revert the rename
/// back to the feed document's own `<title>`.
pub fn rename_feed(conn: &Connection, id: i64, title: &str) -> AppResult<()> {
    // Reject an empty/whitespace-only title at the chokepoint. A rename also
    // sets `custom_title = 1`, which makes `update_feed_meta`
    // never again overwrite the title from the feed document — so an empty
    // title would leave the feed *permanently* blank in the sidebar with no
    // recovery path, not even a refresh. The frontend `PromptDialog` guards
    // against this, but the backend command is the real chokepoint (other IPC
    // callers exist), so enforce it here the way `rename_tag` already does.
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::code("emptyFeedTitle"));
    }
    conn.execute(
        "UPDATE feeds SET title = ?2, custom_title = 1 WHERE id = ?1",
        params![id, title],
    )?;
    Ok(())
}

// ─────────────────────────── newsletter sources ───────────────────────────

/// One configured email-newsletter source: the backing feed plus its IMAP
/// connection details. Mirrors the `commands::NewsletterSource` payload.
pub struct NewsletterSourceRow {
    pub feed_id: i64,
    pub title: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub folder: String,
}

/// Insert a newsletter source: a `feeds` row (source_type = 'newsletter') plus
/// its IMAP credentials in `newsletter_sources`. Both land in one transaction
/// so a failure cannot leave a feed with no credentials. Returns the feed id.
pub fn insert_newsletter_source(
    conn: &Connection,
    feed_url: &str,
    title: &str,
    cfg: &crate::ingestion::newsletter::NewsletterConfig,
) -> AppResult<i64> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO feeds(feed_url, title, source_type) VALUES (?1, ?2, 'newsletter')",
        params![feed_url, title],
    )?;
    let feed_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO newsletter_sources(feed_id, host, port, username, password, folder)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![feed_id, cfg.host, cfg.port, cfg.username, cfg.password, cfg.folder],
    )?;
    tx.commit()?;
    Ok(feed_id)
}

/// Every configured newsletter source (without the password) for the UI list.
pub fn list_newsletter_sources(conn: &Connection) -> AppResult<Vec<NewsletterSourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT n.feed_id, f.title, n.host, n.port, n.username, n.folder
         FROM newsletter_sources n JOIN feeds f ON f.id = n.feed_id
         ORDER BY f.title COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(NewsletterSourceRow {
                feed_id: r.get(0)?,
                title: r.get(1)?,
                host: r.get(2)?,
                port: r.get::<_, i64>(3)? as u16,
                username: r.get(4)?,
                folder: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for every newsletter source — the work list the
/// refresh scheduler polls each cycle. Returning a `NewsletterConfig` directly
/// spares the caller a field-by-field rebuild.
pub fn newsletter_sources_to_poll(
    conn: &Connection,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT feed_id, host, port, username, password, folder FROM newsletter_sources",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for newsletter sources that are *due* to be polled,
/// applying the same per-feed/global interval logic as `feeds_due_for_refresh`.
/// Used by the background scheduler; the manual refresh polls every mailbox.
pub fn newsletter_sources_due_to_poll(
    conn: &Connection,
    global_min: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT s.feed_id, s.host, s.port, s.username, s.password, s.folder
         FROM newsletter_sources s JOIN feeds f ON f.id = s.feed_id
         WHERE COALESCE(f.refresh_interval_min, ?1) < ?2
           AND ( f.last_fetched_at IS NULL
                 OR (julianday('now') - julianday(f.last_fetched_at)) * 1440.0
                    >= COALESCE(f.refresh_interval_min, ?1) )",
    )?;
    let rows = stmt
        .query_map(params![global_min, REFRESH_OFF_MINUTES], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for a single newsletter source — the per-feed
/// manual refresh. Empty when the feed id is not a newsletter source.
pub fn newsletter_sources_for_feed(
    conn: &Connection,
    feed_id: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT feed_id, host, port, username, password, folder
         FROM newsletter_sources WHERE feed_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![feed_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// `(feed_id, IMAP config)` for the newsletter sources in a folder — the
/// per-folder manual refresh.
pub fn newsletter_sources_in_folder(
    conn: &Connection,
    folder_id: i64,
) -> AppResult<Vec<(i64, crate::ingestion::newsletter::NewsletterConfig)>> {
    use crate::ingestion::newsletter::NewsletterConfig;
    let mut stmt = conn.prepare(
        "SELECT s.feed_id, s.host, s.port, s.username, s.password, s.folder
         FROM newsletter_sources s JOIN feeds f ON f.id = s.feed_id
         WHERE f.folder_id = ?1",
    )?;
    let rows = stmt
        .query_map(params![folder_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                NewsletterConfig {
                    host: r.get::<_, String>(1)?,
                    port: r.get::<_, i64>(2)? as u16,
                    username: r.get::<_, String>(3)?,
                    password: r.get::<_, String>(4)?,
                    folder: r.get::<_, String>(5)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Remove a newsletter source. Deleting the `feeds` row cascades to both
/// `newsletter_sources` and the source's articles.
pub fn delete_newsletter_source(conn: &Connection, feed_id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM feeds WHERE id = ?1", params![feed_id])?;
    Ok(())
}

// ─────────────────────────── articles ───────────────────────────

/// A parsed article ready for insertion.
pub struct NewArticle {
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content_html: Option<String>,
    pub body_text: String,
    pub image_url: Option<String>,
    pub published_at: Option<String>,
    pub enclosures: Vec<Enclosure>,
}

/// True if `rule` (scoped to `feed_id`) matches the incoming article `a`.
/// The query is a comma-separated keyword list; any substring hit fires it.
fn rule_matches(rule: &Rule, feed_id: i64, a: &NewArticle) -> bool {
    if rule.feed_id.is_some_and(|fid| fid != feed_id) {
        return false;
    }
    let author = a.author.as_deref().unwrap_or("");
    // The fields the rule searches. `any` checks each field *independently*
    // (mirroring `preview_rule`'s per-column LIKE): a keyword must lie wholly
    // within one field. Concatenating the fields would let a keyword straddle
    // a field boundary (e.g. a title ending in "machine" + a body starting
    // with "learning" matching "machine learning"), so live ingestion would
    // act on articles the rule preview never counted.
    let fields: Vec<String> = match rule.field.as_str() {
        "author" => vec![author.to_lowercase()],
        "content" => vec![a.body_text.to_lowercase()],
        "any" => vec![
            a.title.to_lowercase(),
            author.to_lowercase(),
            a.body_text.to_lowercase(),
        ],
        _ => vec![a.title.to_lowercase()],
    };
    rule.query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .any(|term| fields.iter().any(|h| h.contains(&term)))
}

/// Insert an article if it is new (by feed_id + guid). Returns `true` only when
/// a genuinely **new and unread** article was inserted — callers tally this as
/// the count of fresh articles surfaced to the user (refresh toast, "new
/// articles" notification, `add_newsletter_source`'s `unread_count`).
///
/// An article inserted but pre-marked read by a `read` rule returns `false`:
/// the row landed, but it never shows up as unread, so counting it would
/// inflate the "N new articles" figure and disagree with the sidebar's unread
/// count (the same overcount `add_feed` guards against).
///
/// When `dedup` is on, an article whose URL already exists (in any feed) is
/// skipped — collapsing the same story pushed by multiple feeds. Enabled
/// `rules` are evaluated first: a `skip` match drops the article entirely,
/// while `read` / `star` matches pre-set the article's state on insert.
pub fn upsert_article(
    conn: &Connection,
    feed_id: i64,
    a: &NewArticle,
    dedup: bool,
    rules: &[Rule],
) -> AppResult<bool> {
    if dedup {
        if let Some(url) = a.url.as_deref().filter(|u| !u.is_empty()) {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM articles WHERE url = ?1)",
                params![url],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(false);
            }
        }
    }
    // Apply filter rules: skip wins outright; read / star tint the new row.
    let (mut start_read, mut start_starred) = (false, false);
    for rule in rules {
        if !rule_matches(rule, feed_id, a) {
            continue;
        }
        match rule.action.as_str() {
            "skip" => return Ok(false),
            "read" => start_read = true,
            "star" => start_starred = true,
            _ => {}
        }
    }
    // The article row, its FTS index entry, and its enclosures must land
    // together — a partial insert leaves an unsearchable or enclosure-less
    // article. Wrap them in a transaction so a mid-loop failure rolls back.
    let tx = conn.unchecked_transaction()?;
    let n = tx.execute(
        "INSERT INTO articles
            (feed_id, guid, url, title, author, summary, content_html, body_text,
             image_url, published_at, is_read, is_starred)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(feed_id, guid) DO NOTHING",
        params![
            feed_id, a.guid, a.url, a.title, a.author, a.summary,
            a.content_html, a.body_text, a.image_url, a.published_at,
            start_read, start_starred
        ],
    )?;
    if n == 0 {
        return Ok(false);
    }
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO articles_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
        params![id, a.title, a.body_text],
    )?;
    for e in &a.enclosures {
        tx.execute(
            "INSERT INTO enclosures(article_id, url, mime_type, length) VALUES (?1,?2,?3,?4)",
            params![id, e.url, e.mime_type, e.length],
        )?;
    }
    tx.commit()?;
    // A row inserted but pre-marked read by a `read` rule is not "new" from
    // the user's point of view — report it as not-inserted so it is excluded
    // from new-article tallies.
    Ok(!start_read)
}

/// Build and run the article-list query for the given sidebar selection.
pub fn list_articles(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
    search: Option<&str>,
    oldest_first: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    let mut where_clauses: Vec<String> = vec!["1=1".into()];
    let mut binds: Vec<Value> = Vec::new();

    match query {
        ArticleQuery::All => {}
        ArticleQuery::Unread => where_clauses.push("a.is_read = 0".into()),
        ArticleQuery::Starred => where_clauses.push("a.is_starred = 1".into()),
        ArticleQuery::ReadLater => where_clauses.push("a.read_later = 1".into()),
        ArticleQuery::Feed(id) => {
            where_clauses.push("a.feed_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Folder(id) => {
            where_clauses.push("f.folder_id = ?".into());
            binds.push(Value::Integer(*id));
        }
        ArticleQuery::Tag(id) => {
            where_clauses.push(
                "a.id IN (SELECT article_id FROM article_tags WHERE tag_id = ?)".into(),
            );
            binds.push(Value::Integer(*id));
        }
    }
    if unread_only && !matches!(query, ArticleQuery::Unread) {
        where_clauses.push("a.is_read = 0".into());
    }

    let searching = search.map(|s| !s.trim().is_empty()).unwrap_or(false);
    let mut sql = String::from(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author,
                substr(a.body_text,1,280), a.image_url, a.url, a.published_at,
                a.is_read, a.is_starred, a.read_later
         FROM articles a JOIN feeds f ON f.id = a.feed_id ",
    );
    if searching {
        sql.push_str("JOIN articles_fts fts ON fts.rowid = a.id ");
        where_clauses.push("articles_fts MATCH ?".into());
        // Explicit search: every typed word must match (AND), so results
        // narrow as the user adds terms.
        binds.push(Value::Text(fts_query(search.unwrap(), false)));
    }
    sql.push_str("WHERE ");
    sql.push_str(&where_clauses.join(" AND "));
    // Sort by the effective date — COALESCE(published_at, fetched_at) — so
    // an article with no feed-supplied date orders by when it arrived rather
    // than sinking to the bottom. The two columns are stored in different
    // textual formats (`published_at` RFC 3339 with a `T`; `fetched_at` the
    // space form), so a raw string compare mis-orders a list mixing both;
    // `datetime()` normalises each side to a single comparable form. Backed
    // by `idx_articles_sort`, an expression index over the same wrapped
    // expression (v12) — the planner uses it for both directions, no sort.
    sql.push_str(if searching {
        " ORDER BY fts.rank "
    } else if oldest_first {
        " ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) ASC, a.id ASC "
    } else {
        " ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC "
    });
    sql.push_str("LIMIT ? OFFSET ?");
    binds.push(Value::Integer(limit));
    binds.push(Value::Integer(offset));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(binds), |r| {
            Ok(ArticleSummary {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                snippet: r.get(6)?,
                image_url: r.get(7)?,
                url: r.get(8)?,
                published_at: r.get(9)?,
                is_read: r.get(10)?,
                is_starred: r.get(11)?,
                read_later: r.get(12)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Scan stored articles that have no thumbnail but whose feed or extracted
/// body HTML embeds an image, returning the `(id, image_url)` pairs to adopt.
/// Reads only — paired with `apply_card_images` so the caller can run this
/// heavy parse on a reader connection and the quick writes under the writer
/// lock. One-time: feeds ingested after the body-image fallback shipped
/// already store this at parse.
pub fn card_image_backfill_scan(conn: &Connection) -> AppResult<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content_html, extracted_html FROM articles
         WHERE (image_url IS NULL OR trim(image_url) = '')
           AND (
                (content_html IS NOT NULL AND content_html <> '')
                OR (extracted_html IS NOT NULL AND extracted_html <> '')
           )",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, content_html, extracted_html) = row?;
        let img = content_html
            .as_deref()
            .and_then(crate::sanitize::first_image)
            .or_else(|| extracted_html.as_deref().and_then(crate::sanitize::first_image));
        if let Some(img) = img {
            out.push((id, img));
        }
    }
    Ok(out)
}

/// Persist the `(id, image_url)` pairs found by `card_image_backfill_scan`,
/// in a single transaction.
pub fn apply_card_images(conn: &Connection, updates: &[(i64, String)]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    for (id, img) in updates {
        tx.execute(
            "UPDATE articles SET image_url = ?2 WHERE id = ?1",
            params![id, img],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Turn raw user text into a safe FTS5 MATCH expression (each term
/// prefix-matched). `or_join` selects how multiple terms combine: `false`
/// joins them with an implicit AND (every term must match — explicit search,
/// where adding words narrows results); `true` joins them with `OR` (any term
/// may match — recall-oriented retrieval, e.g. RAG over a natural-language
/// question, where AND-ing every word would match nothing).
///
/// Punctuation *splits* a word into separate terms rather than being deleted
/// inside it: the `unicode61` tokenizer indexes `rust-lang` / `node.js` as the
/// two tokens `rust`+`lang` and `node`+`js`, so collapsing the query side to
/// `rustlang` / `nodejs` would match nothing the index actually holds.
fn fts_query(input: &str, or_join: bool) -> String {
    let terms: Vec<String> = input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    if terms.is_empty() {
        "\"\"".into()
    } else if or_join {
        terms.join(" OR ")
    } else {
        terms.join(" ")
    }
}

/// Retrieve up to `limit` articles relevant to a natural-language `question`,
/// for use as RAG context. Uses OR-joined FTS terms so a multi-word question
/// still matches articles that contain *some* of its keywords — an AND join
/// (as explicit search uses) would require every word to appear and so return
/// nothing for a real question. Returns `(id, title, feed_title)` ordered by
/// FTS relevance. An all-stopword / punctuation-only question yields no rows.
pub fn search_articles_for_rag(
    conn: &Connection,
    question: &str,
    limit: i64,
) -> AppResult<Vec<(i64, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.title, f.title
         FROM articles a
         JOIN feeds f ON f.id = a.feed_id
         JOIN articles_fts fts ON fts.rowid = a.id
         WHERE articles_fts MATCH ?1
         ORDER BY fts.rank
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![fts_query(question, true), limit], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Recent articles as `(title, feed_title, text)` for building an AI digest.
pub fn digest_source(conn: &Connection, limit: i64) -> AppResult<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT a.title, f.title, substr(a.body_text, 1, 600)
         FROM articles a JOIN feeds f ON f.id = a.feed_id
         ORDER BY datetime(COALESCE(a.published_at, a.fetched_at)) DESC, a.id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_article(conn: &Connection, id: i64) -> AppResult<ArticleDetail> {
    let mut detail = conn.query_row(
        "SELECT a.id, a.feed_id, f.title, f.source_type, a.title, a.author, a.url,
                a.content_html, a.extracted_html, a.image_url, a.published_at,
                a.is_read, a.is_starred, a.read_later, a.ai_summary,
                a.translated_html, a.translated_lang
         FROM articles a JOIN feeds f ON f.id = a.feed_id WHERE a.id = ?1",
        params![id],
        |r| {
            Ok(ArticleDetail {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                feed_title: r.get(2)?,
                source_type: r.get(3)?,
                title: r.get(4)?,
                author: r.get(5)?,
                url: r.get(6)?,
                content_html: r.get(7)?,
                extracted_html: r.get(8)?,
                image_url: r.get(9)?,
                published_at: r.get(10)?,
                is_read: r.get(11)?,
                is_starred: r.get(12)?,
                read_later: r.get(13)?,
                ai_summary: r.get(14)?,
                translated_html: r.get(15)?,
                translated_lang: r.get(16)?,
                enclosures: Vec::new(),
                tags: Vec::new(),
            })
        },
    )?;
    let mut stmt =
        conn.prepare("SELECT url, mime_type, length FROM enclosures WHERE article_id = ?1")?;
    detail.enclosures = stmt
        .query_map(params![id], |r| {
            Ok(Enclosure {
                url: r.get(0)?,
                mime_type: r.get(1)?,
                length: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    detail.tags = tags_for_article(conn, id)?;
    Ok(detail)
}

/// `(title, plain_text)` for building an AI prompt. Prefers the extracted
/// full text when the user has run extraction, so a summary / answer covers
/// the whole article rather than the (often truncated) feed body.
pub fn article_text(conn: &Connection, id: i64) -> AppResult<(String, String)> {
    let (title, body, extracted): (String, String, Option<String>) = conn.query_row(
        "SELECT title, body_text, extracted_html FROM articles WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let text = match extracted {
        Some(html) if !html.trim().is_empty() => crate::sanitize::html_to_text(&html),
        _ => body,
    };
    Ok((title, text))
}

pub fn set_read(conn: &Connection, id: i64, read: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_read = ?2 WHERE id = ?1", params![id, read])?;
    Ok(())
}

pub fn set_starred(conn: &Connection, id: i64, starred: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET is_starred = ?2 WHERE id = ?1", params![id, starred])?;
    Ok(())
}

pub fn set_read_later(conn: &Connection, id: i64, v: bool) -> AppResult<()> {
    conn.execute("UPDATE articles SET read_later = ?2 WHERE id = ?1", params![id, v])?;
    Ok(())
}

/// Store the extracted full-text HTML and re-index the article's FTS body
/// with it, so search covers the whole article rather than just the short
/// summary the feed shipped.
pub fn set_extracted_html(
    conn: &Connection,
    id: i64,
    html: &str,
    image_url: Option<&str>,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE articles
            SET extracted_html = ?2,
                image_url = CASE
                    WHEN ?3 IS NOT NULL AND (image_url IS NULL OR trim(image_url) = '')
                    THEN ?3
                    ELSE image_url
                END
         WHERE id = ?1",
        params![id, html, image_url],
    )?;
    tx.execute(
        "UPDATE articles_fts SET body = ?2 WHERE rowid = ?1",
        params![id, crate::sanitize::html_to_text(html)],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_ai_summary(conn: &Connection, id: i64, summary: &str) -> AppResult<()> {
    conn.execute("UPDATE articles SET ai_summary = ?2 WHERE id = ?1", params![id, summary])?;
    Ok(())
}

/// Cache a completed translation of an article's body, tagged with the target
/// language it was produced for so a later language change is detected as stale.
pub fn set_translation(conn: &Connection, id: i64, html: &str, lang: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET translated_html = ?2, translated_lang = ?3 WHERE id = ?1",
        params![id, html, lang],
    )?;
    Ok(())
}

/// Mark every article matching the current sidebar selection as read. When
/// `enqueue_sync` is set, the read change is also queued for the sync server
/// — otherwise a bulk mark-all-read never reaches FreshRSS and the next pull
/// silently reverts it.
pub fn mark_all_read(
    conn: &Connection,
    query: &ArticleQuery,
    enqueue_sync: bool,
) -> AppResult<usize> {
    // WHERE fragment selecting the articles in the current view, plus an
    // optional bound id (feed / folder / tag). `pred` is a fixed literal.
    let (pred, id): (&str, Option<i64>) = match query {
        ArticleQuery::All | ArticleQuery::Unread => ("1", None),
        ArticleQuery::Starred => ("is_starred = 1", None),
        ArticleQuery::ReadLater => ("read_later = 1", None),
        ArticleQuery::Feed(id) => ("feed_id = ?1", Some(*id)),
        ArticleQuery::Folder(id) => (
            "feed_id IN (SELECT id FROM feeds WHERE folder_id = ?1)",
            Some(*id),
        ),
        ArticleQuery::Tag(id) => (
            "id IN (SELECT article_id FROM article_tags WHERE tag_id = ?1)",
            Some(*id),
        ),
    };
    let bind: Vec<&dyn rusqlite::ToSql> =
        id.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    // Queue + flip together: the sync-queue rows and the is_read change must
    // commit atomically, or a mid-way failure leaves the queue claiming a
    // read state the articles never reached. Queue *before* flipping so the
    // `is_read = 0` filter still matches; the SELECT's WHERE also
    // disambiguates the ON CONFLICT clause.
    //
    // Articles are queued regardless of whether they already carry a
    // `remote_id`: freshly fetched items have none until a pull matches them
    // by URL, and "mark all read" is most often run right after a refresh on
    // exactly those items. `take_sync_queue` defers any entry whose article
    // still lacks a remote id, so the change pushes on the sync after the id
    // is assigned — mirroring the single-article `enqueue_sync` path. The old
    // `remote_id IS NOT NULL` filter here silently dropped those changes.
    let tx = conn.unchecked_transaction()?;
    if enqueue_sync {
        tx.execute(
            &format!(
                "INSERT INTO sync_queue(article_id, field, value)
                 SELECT id, 'read', 1 FROM articles
                 WHERE {pred} AND is_read = 0
                 ON CONFLICT(article_id, field) DO UPDATE SET value = 1"
            ),
            bind.as_slice(),
        )?;
    }
    let n = tx.execute(
        &format!("UPDATE articles SET is_read = 1 WHERE {pred} AND is_read = 0"),
        bind.as_slice(),
    )?;
    tx.commit()?;
    Ok(n)
}

/// Whether a FreshRSS server is currently linked (a non-empty URL is stored).
pub fn is_freshrss_connected(conn: &Connection) -> bool {
    get_setting(conn, "freshrss_url")
        .ok()
        .flatten()
        .map(|u| !u.trim().is_empty())
        .unwrap_or(false)
}

// ─────────────────────────── tags ───────────────────────────

/// Palette keys cycled through as new tags are created.
const TAG_COLORS: &[&str] = &[
    "clay", "amber", "pine", "teal", "indigo", "violet", "rose", "slate",
];

/// Every tag, ordered for the sidebar, with a live article count.
pub fn list_tags(conn: &Connection) -> AppResult<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.position,
                (SELECT COUNT(*) FROM article_tags at WHERE at.tag_id = t.id)
         FROM tags t ORDER BY t.position, t.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                article_count: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Create a tag, auto-assigning the next palette colour and list position.
///
/// Idempotent on name: `tags.name` is `UNIQUE`, so a plain `INSERT` of an
/// existing name would fail the constraint. Both call sites — the sidebar's
/// "new tag" prompt and the reader's create-and-attach picker — treat a name
/// the user typed, which may already exist (the picker even lists every tag
/// right above the input). Matching is case-insensitive, consistent with how
/// `list_tags` orders names, so "Rust" and "rust" resolve to one tag rather
/// than silently diverging. An existing name returns that tag's id instead of
/// erroring.
pub fn create_tag(conn: &Connection, name: &str) -> AppResult<i64> {
    // Trim before the dedup lookup and the insert: a name with surrounding
    // whitespace is a distinct string from its trimmed twin, so the
    // `COLLATE NOCASE` lookup would miss the existing tag and spawn a
    // near-duplicate. Normalise at this one chokepoint so the invariant holds
    // regardless of caller-side trimming. Mirrors `create_folder`.
    let name = name.trim();
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    // Position the new tag at the end of the list. `MAX(position)+1` — not
    // `COUNT(*)` — is required: deleting a tag from the middle leaves a gap,
    // so a fresh `COUNT(*)` would collide with an existing tag's position and
    // the new tag would not sort last (only the name tiebreaker would save
    // it). The colour cycles off the same index so the palette stays varied.
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM tags",
        [],
        |r| r.get(0),
    )?;
    let color = TAG_COLORS[(next as usize) % TAG_COLORS.len()];
    conn.execute(
        "INSERT INTO tags(name, color, position) VALUES (?1, ?2, ?3)",
        params![name, color, next],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a tag, rejecting a name that collides with a *different* existing
/// tag.
///
/// `tags.name` is `UNIQUE` (case-sensitively). Without this guard a rename to a
/// name that exactly matches another tag would fail the constraint and surface
/// the raw SQLite message to the user; and a rename to a *case variant* of
/// another tag ("rust" → "Rust") would succeed and create the near-duplicate
/// that `create_tag` deliberately collapses — leaving the two functions
/// inconsistent. Match case-insensitively, the same basis `create_tag` and
/// `list_tags`'s ordering use, and return a localisable `tagNameExists` code.
/// Renaming a tag to its own current name (or a case change of it) is allowed.
pub fn rename_tag(conn: &Connection, id: i64, name: &str) -> AppResult<()> {
    // Trim so the collision check and the stored value match what `create_tag`
    // would produce — otherwise a rename to a whitespace-padded variant slips
    // past the clash test and recreates the near-duplicate.
    let name = name.trim();
    let clash: Option<i64> = conn
        .query_row(
            "SELECT id FROM tags WHERE name = ?1 COLLATE NOCASE AND id != ?2",
            params![name, id],
            |r| r.get(0),
        )
        .optional()?;
    if clash.is_some() {
        return Err(AppError::code("tagNameExists"));
    }
    conn.execute("UPDATE tags SET name = ?2 WHERE id = ?1", params![id, name])?;
    Ok(())
}

pub fn set_tag_color(conn: &Connection, id: i64, color: &str) -> AppResult<()> {
    conn.execute("UPDATE tags SET color = ?2 WHERE id = ?1", params![id, color])?;
    Ok(())
}

/// Persist a new tag ordering — `ids` listed in the desired display order.
/// The per-row updates run in one transaction so a mid-loop failure can't
/// leave the tag list in a half-reordered state.
pub fn reorder_tags(conn: &Connection, ids: &[i64]) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    for (pos, id) in ids.iter().enumerate() {
        tx.execute(
            "UPDATE tags SET position = ?2 WHERE id = ?1",
            params![id, pos as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_tag(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM tags WHERE id = ?1", params![id])?;
    Ok(())
}

/// Attach (`on = true`) or detach a tag from one article.
pub fn set_article_tag(conn: &Connection, article_id: i64, tag_id: i64, on: bool) -> AppResult<()> {
    if on {
        conn.execute(
            "INSERT INTO article_tags(article_id, tag_id) VALUES (?1, ?2)
             ON CONFLICT DO NOTHING",
            params![article_id, tag_id],
        )?;
    } else {
        conn.execute(
            "DELETE FROM article_tags WHERE article_id = ?1 AND tag_id = ?2",
            params![article_id, tag_id],
        )?;
    }
    Ok(())
}

/// Tags attached to one article (article_count left at 0 — unused per-article).
pub fn tags_for_article(conn: &Connection, article_id: i64) -> AppResult<Vec<Tag>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.name, t.color, t.position
         FROM tags t JOIN article_tags at ON at.tag_id = t.id
         WHERE at.article_id = ?1 ORDER BY t.position, t.name COLLATE NOCASE",
    )?;
    let rows = stmt
        .query_map(params![article_id], |r| {
            Ok(Tag {
                id: r.get(0)?,
                name: r.get(1)?,
                color: r.get(2)?,
                position: r.get(3)?,
                article_count: 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── filter rules ───────────────────────────

fn row_to_rule(r: &rusqlite::Row) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        name: r.get(1)?,
        enabled: r.get(2)?,
        feed_id: r.get(3)?,
        field: r.get(4)?,
        query: r.get(5)?,
        action: r.get(6)?,
        position: r.get(7)?,
    })
}

const RULE_COLS: &str = "id, name, enabled, feed_id, field, query, action, position";

/// Every rule, enabled or not, ordered for the settings list.
pub fn list_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt =
        conn.prepare(&format!("SELECT {RULE_COLS} FROM rules ORDER BY position, id"))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Only the enabled rules — the set evaluated against incoming articles.
pub fn active_rules(conn: &Connection) -> AppResult<Vec<Rule>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RULE_COLS} FROM rules WHERE enabled = 1 ORDER BY position, id"
    ))?;
    let rows = stmt
        .query_map([], row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_rule(
    conn: &Connection,
    name: &str,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<i64> {
    // Position the new rule at the end. `MAX(position)+1` — not `COUNT(*)` —
    // is required: deleting a rule from the middle leaves a gap, so a fresh
    // `COUNT(*)` would collide with an existing rule's position and the new
    // rule would not sort last (`ORDER BY position, id` would then slot it
    // before any later-positioned rule). Rule order is semantically load-
    // bearing — `active_rules` evaluates in this order and a `skip` match
    // short-circuits — so a stale position can change which action fires.
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(position), -1) + 1 FROM rules",
        [],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO rules(name, feed_id, field, query, action, position)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![name, feed_id, field, query, action, next],
    )?;
    Ok(conn.last_insert_rowid())
}

#[allow(clippy::too_many_arguments)]
pub fn update_rule(
    conn: &Connection,
    id: i64,
    name: &str,
    enabled: bool,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<()> {
    conn.execute(
        "UPDATE rules SET name = ?2, enabled = ?3, feed_id = ?4,
                          field = ?5, query = ?6, action = ?7
         WHERE id = ?1",
        params![id, name, enabled, feed_id, field, query, action],
    )?;
    Ok(())
}

pub fn delete_rule(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM rules WHERE id = ?1", params![id])?;
    Ok(())
}

/// Build the `WHERE` fragment (and its bind values) that selects the articles a
/// rule matches: one `unicode_lower(col) LIKE ?` per (keyword × searched
/// column), OR-joined, optionally scoped to one feed. Columns are *unaliased*
/// so the fragment slots straight into a bare `SELECT … FROM articles`,
/// `UPDATE articles` or `DELETE FROM articles`. Returns `None` when the query
/// holds no usable keywords (a no-op rule), so callers can short-circuit.
///
/// `preview_rule` (count + samples) and `apply_rule_to_existing` (act) share
/// this builder so the number the preview shows is exactly the set the apply
/// touches. LIKE wildcards in a keyword are escaped so a literal `%` / `_`
/// can't widen the match; the column side is folded with `unicode_lower` (not
/// SQLite's ASCII-only `LOWER`) so it matches the Unicode-aware
/// `to_lowercase()` `rule_matches` applies at ingestion — otherwise a keyword
/// like `café` would be counted here but its `CAFÉ` articles missed, diverging
/// the preview/apply from live ingestion.
fn rule_match_where(
    field: &str,
    query: &str,
    feed_id: Option<i64>,
) -> Option<(String, Vec<Value>)> {
    let terms: Vec<String> = query
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }
    let cols: &[&str] = match field {
        "author" => &["author"],
        "content" => &["body_text"],
        "any" => &["title", "author", "body_text"],
        _ => &["title"],
    };
    let mut ors: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = Vec::new();
    for term in &terms {
        let escaped = term
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        for col in cols {
            ors.push(format!("unicode_lower(COALESCE({col},'')) LIKE ? ESCAPE '\\'"));
            binds.push(Value::Text(format!("%{escaped}%")));
        }
    }
    let mut where_sql = format!("({})", ors.join(" OR "));
    if let Some(fid) = feed_id {
        where_sql.push_str(" AND feed_id = ?");
        binds.push(Value::Integer(fid));
    }
    Some((where_sql, binds))
}

/// Preview how a draft rule would behave: the number of *already-stored*
/// articles its keywords match, plus a handful of recent sample titles.
/// Lets the user sanity-check a rule before saving it.
pub fn preview_rule(
    conn: &Connection,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
) -> AppResult<(i64, Vec<String>)> {
    let Some((where_sql, binds)) = rule_match_where(field, query, feed_id) else {
        return Ok((0, Vec::new()));
    };
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM articles WHERE {where_sql}"),
        params_from_iter(binds.iter().cloned()),
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT title FROM articles WHERE {where_sql}
         ORDER BY datetime(COALESCE(published_at, fetched_at)) DESC, id DESC
         LIMIT 5"
    ))?;
    let samples = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((count, samples))
}

/// Apply a saved rule's action to the articles already in the store that it
/// matches — the one-time backfill run when a rule is created or edited so it
/// affects the existing backlog, not only articles fetched afterwards. Returns
/// the number of articles acted on.
///
/// `skip` *deletes* its matches — the stored-article equivalent of dropping the
/// article at ingestion. FK `ON DELETE CASCADE` (enclosures, highlights, tags)
/// and the `articles_fts_ad` trigger keep dependent rows and the FTS index in
/// sync, the same path retention cleanup relies on. `read` / `star` set the
/// matching flag and skip rows that already carry it, so the returned count is
/// the number of rows actually changed.
pub fn apply_rule_to_existing(
    conn: &Connection,
    feed_id: Option<i64>,
    field: &str,
    query: &str,
    action: &str,
) -> AppResult<usize> {
    let Some((where_sql, binds)) = rule_match_where(field, query, feed_id) else {
        return Ok(0);
    };
    let sql = match action {
        "skip" => format!("DELETE FROM articles WHERE {where_sql}"),
        "read" => {
            format!("UPDATE articles SET is_read = 1 WHERE ({where_sql}) AND is_read = 0")
        }
        "star" => {
            format!("UPDATE articles SET is_starred = 1 WHERE ({where_sql}) AND is_starred = 0")
        }
        _ => return Ok(0),
    };
    Ok(conn.execute(&sql, params_from_iter(binds))?)
}

/// (total unread, starred, read-later) counts for the sidebar smart folders.
pub fn smart_counts(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    Ok(conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM articles WHERE is_read = 0),
            (SELECT COUNT(*) FROM articles WHERE is_starred = 1),
            (SELECT COUNT(*) FROM articles WHERE read_later = 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

// ─────────────────────────── highlights ───────────────────────────

const HIGHLIGHT_COLS: &str =
    "id, article_id, quote, prefix, suffix, text_offset, color, note, created_at";

fn row_to_highlight(r: &rusqlite::Row) -> rusqlite::Result<Highlight> {
    Ok(Highlight {
        id: r.get(0)?,
        article_id: r.get(1)?,
        quote: r.get(2)?,
        prefix: r.get(3)?,
        suffix: r.get(4)?,
        text_offset: r.get(5)?,
        color: r.get(6)?,
        note: r.get(7)?,
        created_at: r.get(8)?,
    })
}

/// The fields needed to create a highlight — everything in [`Highlight`]
/// except the database-assigned `id` and `created_at`. Grouping the anchor
/// fields (which are all `&str` and otherwise trivially swappable) into one
/// named value keeps `insert_highlight` calls unambiguous.
pub struct NewHighlight<'a> {
    pub article_id: i64,
    pub quote: &'a str,
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub text_offset: i64,
    pub color: &'a str,
    pub note: &'a str,
}

/// Insert a highlight and return its new id.
pub fn insert_highlight(conn: &Connection, h: &NewHighlight) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO highlights(article_id, quote, prefix, suffix, text_offset, color, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            h.article_id,
            h.quote,
            h.prefix,
            h.suffix,
            h.text_offset,
            h.color,
            h.note
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// All highlights for one article, oldest first (their reading order).
pub fn list_highlights(conn: &Connection, article_id: i64) -> AppResult<Vec<Highlight>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HIGHLIGHT_COLS} FROM highlights
         WHERE article_id = ?1 ORDER BY text_offset, id"
    ))?;
    let rows = stmt
        .query_map(params![article_id], row_to_highlight)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Every highlight across all articles — used by the Highlights browser.
pub fn list_all_highlights(conn: &Connection) -> AppResult<Vec<Highlight>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HIGHLIGHT_COLS} FROM highlights ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = stmt
        .query_map([], row_to_highlight)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Fetch one highlight by id, if it exists.
#[allow(dead_code)] // exercised by the db tests; kept as a complete CRUD API.
pub fn get_highlight(conn: &Connection, id: i64) -> AppResult<Option<Highlight>> {
    Ok(conn
        .query_row(
            &format!("SELECT {HIGHLIGHT_COLS} FROM highlights WHERE id = ?1"),
            params![id],
            row_to_highlight,
        )
        .optional()?)
}

/// Replace a highlight's note text (an empty string clears it).
pub fn update_highlight_note(conn: &Connection, id: i64, note: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE highlights SET note = ?2 WHERE id = ?1",
        params![id, note],
    )?;
    Ok(())
}

/// Change a highlight's colour (a palette key).
pub fn set_highlight_color(conn: &Connection, id: i64, color: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE highlights SET color = ?2 WHERE id = ?1",
        params![id, color],
    )?;
    Ok(())
}

pub fn delete_highlight(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM highlights WHERE id = ?1", params![id])?;
    Ok(())
}

// ─────────────────────────── settings ───────────────────────────

pub fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| {
            r.get(0)
        })
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Read a setting and parse it as `T`, falling back to `default` when the key
/// is missing, unreadable, or fails to parse.
pub fn setting_parsed<T: std::str::FromStr>(conn: &Connection, key: &str, default: T) -> T {
    get_setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read a setting as a boolean flag — `"1"` and `"true"` are true, anything
/// else (including a missing key) falls back to `default`.
pub fn setting_flag(conn: &Connection, key: &str, default: bool) -> bool {
    get_setting(conn, key)
        .ok()
        .flatten()
        .map(|v| v == "1" || v == "true")
        .unwrap_or(default)
}

// ─────────────────────────── storage ───────────────────────────

/// `(database bytes, article count, feed count)` for the storage panel.
pub fn storage_stats(conn: &Connection) -> AppResult<(i64, i64, i64)> {
    let page_count: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    let page_size: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let articles: i64 = conn.query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))?;
    let feeds: i64 = conn.query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))?;
    Ok((page_count * page_size, articles, feeds))
}

/// Delete read articles older than `days`, keeping starred / read-later ones.
/// Returns the number removed. Age is the effective date —
/// COALESCE(published_at, fetched_at) — so a dateless article is retained by
/// fetch age rather than living forever (fetched_at is never NULL).
///
/// The two timestamp columns are stored in different textual formats:
/// `published_at` is RFC 3339 (`2024-01-15T10:30:00+00:00`, written by
/// `to_rfc3339`) while `fetched_at` and `datetime('now', …)` use SQLite's
/// space-separated form (`2024-01-15 10:30:00`). A raw string `<` mis-orders
/// them — the `T` byte sorts *after* a space, so a `published_at` value looks
/// almost a day newer than it is and same-day articles escape the cutoff.
/// Wrapping every side in `datetime()` parses both formats to the canonical
/// representation, so the comparison reflects the real instant.
pub fn cleanup_old_articles(conn: &Connection, days: i64) -> AppResult<usize> {
    // A retention window must be a positive number of days. A non-positive
    // value is meaningless and dangerous: `days = 0` builds the modifier
    // `'-0 days'`, so the cutoff `datetime('now', '-0 days')` collapses to
    // *now* and the DELETE purges **every** read article regardless of age;
    // a negative `days` builds a malformed `'--N days'` modifier that
    // `datetime()` evaluates to NULL, silently deleting nothing. Neither is a
    // real retention policy. The Settings UI only ever offers 30/90/180, but
    // this is the one chokepoint both that command and the background
    // scheduler funnel through — and the scheduler parses `days` from a
    // free-form settings string — so reject a non-positive value here rather
    // than trust every caller. Bail out as a no-op (0 removed).
    if days <= 0 {
        return Ok(0);
    }
    // Retention deletes only articles the user has not signalled they want to
    // keep. Starred and read-later are explicit "keep" flags; an article the
    // user has *highlighted* carries the same intent — the highlights table
    // cascade-deletes with the article (`ON DELETE CASCADE`), so purging a
    // highlighted-but-read article would silently destroy that hand-made
    // annotation layer (feature F7). Exempt any article with highlights.
    Ok(conn.execute(
        "DELETE FROM articles
         WHERE is_starred = 0 AND read_later = 0 AND is_read = 1
           AND NOT EXISTS (SELECT 1 FROM highlights WHERE article_id = articles.id)
           AND datetime(COALESCE(published_at, fetched_at))
               < datetime('now', ?1)",
        params![format!("-{days} days")],
    )?)
}

/// Reclaim free pages — must run outside any transaction.
pub fn vacuum(conn: &Connection) -> AppResult<()> {
    conn.execute_batch("VACUUM")?;
    Ok(())
}

/// Wipe all user content (feeds → articles cascade, folders). Settings are
/// kept. Both deletes commit together so a failure can't leave feeds wiped
/// but folders behind.
pub fn clear_all_data(conn: &Connection) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM feeds", [])?;
    tx.execute("DELETE FROM folders", [])?;
    tx.commit()?;
    Ok(())
}

/// Clear every stored setting.
pub fn reset_settings(conn: &Connection) -> AppResult<()> {
    conn.execute("DELETE FROM settings", [])?;
    Ok(())
}

pub fn count_unread(conn: &Connection) -> AppResult<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM articles WHERE is_read = 0", [], |r| r.get(0))?)
}

/// Unread article count for a single feed — the same expression `list_feeds`
/// computes per row, used by `add_feed` so its returned `unread_count` matches
/// what the sidebar will show (rules that pre-mark an article read must not be
/// counted as unread).
pub fn count_feed_unread(conn: &Connection, feed_id: i64) -> AppResult<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM articles WHERE feed_id = ?1 AND is_read = 0",
        params![feed_id],
        |r| r.get(0),
    )?)
}

/// Timestamp of the most recent successful feed fetch, if any.
pub fn latest_fetch(conn: &Connection) -> AppResult<Option<String>> {
    Ok(conn.query_row("SELECT MAX(last_fetched_at) FROM feeds", [], |r| {
        r.get::<_, Option<String>>(0)
    })?)
}

// ─────────────────────────── sync ───────────────────────────

/// Local article id for a given source URL — used to reconcile remote state.
pub fn article_id_by_url(conn: &Connection, url: &str) -> AppResult<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM articles WHERE url = ?1 LIMIT 1",
            params![url],
            |r| r.get(0),
        )
        .optional()?)
}

pub fn set_remote_id(conn: &Connection, article_id: i64, remote_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET remote_id = ?2 WHERE id = ?1",
        params![article_id, remote_id],
    )?;
    Ok(())
}

/// Apply remote read/starred state to a local article.
pub fn set_sync_state(
    conn: &Connection,
    article_id: i64,
    read: bool,
    starred: bool,
) -> AppResult<()> {
    conn.execute(
        "UPDATE articles SET is_read = ?2, is_starred = ?3 WHERE id = ?1",
        params![article_id, read, starred],
    )?;
    Ok(())
}

/// Queue a local read/starred change to push on the next sync.
pub fn enqueue_sync(
    conn: &Connection,
    article_id: i64,
    field: &str,
    value: bool,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO UPDATE SET value = excluded.value",
        params![article_id, field, value],
    )?;
    Ok(())
}

/// Article ids that still carry un-pushed local changes — their state must not
/// be overwritten by a pull until the change has been sent.
pub fn pending_sync_article_ids(conn: &Connection) -> AppResult<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT DISTINCT article_id FROM sync_queue")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

/// One pushable change drained from the sync queue.
pub struct SyncEntry {
    pub article_id: i64,
    pub remote_id: String,
    pub field: String,
    pub value: bool,
}

/// Drain pushable queue entries. Only rows whose article already has a remote
/// id are returned and removed; the rest wait for a pull to assign one. The
/// caller MUST re-queue any entry whose push fails (see `requeue_sync`) so a
/// network blip never silently drops a local change.
pub fn take_sync_queue(conn: &Connection) -> AppResult<Vec<SyncEntry>> {
    let mut stmt = conn.prepare(
        "SELECT q.article_id, a.remote_id, q.field, q.value
         FROM sync_queue q JOIN articles a ON a.id = q.article_id
         WHERE a.remote_id IS NOT NULL",
    )?;
    let rows: Vec<SyncEntry> = stmt
        .query_map([], |r| {
            Ok(SyncEntry {
                article_id: r.get(0)?,
                remote_id: r.get(1)?,
                field: r.get(2)?,
                value: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    conn.execute(
        "DELETE FROM sync_queue WHERE article_id IN
            (SELECT id FROM articles WHERE remote_id IS NOT NULL)",
        [],
    )?;
    Ok(rows)
}

/// Re-insert a queue entry whose push failed. Unlike `enqueue_sync` this does
/// not clobber a newer edit the user made on the same article during the sync.
pub fn requeue_sync(conn: &Connection, article_id: i64, field: &str, value: bool) -> AppResult<()> {
    conn.execute(
        "INSERT INTO sync_queue(article_id, field, value) VALUES (?1, ?2, ?3)
         ON CONFLICT(article_id, field) DO NOTHING",
        params![article_id, field, value],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory database with all migrations applied and one feed +
    /// article inserted, so highlight FKs resolve. Returns `(conn, article_id)`.
    fn test_db() -> (Connection, i64) {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        MIGRATIONS.to_latest(&mut conn).unwrap();
        // The production `open` / `open_reader` register custom SQL functions;
        // the in-memory test connection must too so `preview_rule`'s
        // `unicode_lower` resolves.
        register_functions(&conn).unwrap();
        let feed_id = insert_feed(
            &conn,
            "https://example.com/feed.xml",
            None,
            "Example Feed",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        let article = NewArticle {
            guid: "g1".into(),
            url: Some("https://example.com/a1".into()),
            title: "An Article".into(),
            author: None,
            summary: None,
            content_html: Some("<p>body</p>".into()),
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &article, false, &[]).unwrap();
        let article_id: i64 = conn
            .query_row("SELECT id FROM articles", [], |r| r.get(0))
            .unwrap();
        (conn, article_id)
    }

    #[test]
    fn due_logic_respects_per_feed_and_global_intervals() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let global = 30;

        // Never fetched → always due, whatever the interval.
        assert!(feeds_due_for_refresh(&conn, global)
            .unwrap()
            .iter()
            .any(|(id, ..)| *id == feed_id));

        // Just fetched → not due under the 30-minute global interval.
        set_feed_fetch_state(&conn, feed_id, None, None, None).unwrap();
        assert!(
            feeds_due_for_refresh(&conn, global).unwrap().is_empty(),
            "a just-fetched feed should not be due"
        );

        // Backdate the fetch 20 minutes: still under the 30-minute global…
        conn.execute(
            "UPDATE feeds SET last_fetched_at = datetime('now', '-20 minutes') WHERE id = ?1",
            params![feed_id],
        )
        .unwrap();
        assert!(feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // …but a 15-minute per-feed override makes it due.
        set_feed_refresh_interval(&conn, feed_id, Some(15)).unwrap();
        assert!(!feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // The "off" sentinel opts the feed out entirely, even when overdue.
        set_feed_refresh_interval(&conn, feed_id, Some(REFRESH_OFF_MINUTES)).unwrap();
        assert!(feeds_due_for_refresh(&conn, global).unwrap().is_empty());

        // Clearing the override returns the feed to the global interval: with
        // the 20-minute-old fetch and a 5-minute global, it is due again.
        set_feed_refresh_interval(&conn, feed_id, None).unwrap();
        assert!(!feeds_due_for_refresh(&conn, 5).unwrap().is_empty());
    }

    #[test]
    fn translation_round_trips_through_get_article() {
        let (conn, id) = test_db();
        // No translation cached on a fresh article.
        let before = get_article(&conn, id).unwrap();
        assert_eq!(before.translated_html, None);
        assert_eq!(before.translated_lang, None);

        set_translation(&conn, id, "<p>译文</p>", "zh").unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.translated_html.as_deref(), Some("<p>译文</p>"));
        assert_eq!(after.translated_lang.as_deref(), Some("zh"));
    }

    #[test]
    fn set_translation_overwrites_a_previous_language() {
        let (conn, id) = test_db();
        set_translation(&conn, id, "<p>译文</p>", "zh").unwrap();
        set_translation(&conn, id, "<p>translation</p>", "en").unwrap();
        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.translated_html.as_deref(), Some("<p>translation</p>"));
        assert_eq!(after.translated_lang.as_deref(), Some("en"));
    }

    #[test]
    fn set_extracted_html_backfills_missing_image_url() {
        let (conn, id) = test_db();
        set_extracted_html(&conn, id, "<p>full text</p>", Some("https://ex.com/lead.jpg"))
            .unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.extracted_html.as_deref(), Some("<p>full text</p>"));
        assert_eq!(after.image_url.as_deref(), Some("https://ex.com/lead.jpg"));
    }

    #[test]
    fn set_extracted_html_keeps_existing_image_url() {
        let (conn, id) = test_db();
        conn.execute(
            "UPDATE articles SET image_url = ?2 WHERE id = ?1",
            params![id, "https://ex.com/feed.jpg"],
        )
        .unwrap();

        set_extracted_html(&conn, id, "<p>full text</p>", Some("https://ex.com/lead.jpg"))
            .unwrap();

        let after = get_article(&conn, id).unwrap();
        assert_eq!(after.image_url.as_deref(), Some("https://ex.com/feed.jpg"));
    }

    #[test]
    fn card_image_backfill_scan_reads_extracted_html() {
        let (conn, id) = test_db();
        set_extracted_html(
            &conn,
            id,
            r#"<p>full text</p><img src="https://ex.com/from-extracted.jpg">"#,
            None,
        )
        .unwrap();

        let updates = card_image_backfill_scan(&conn).unwrap();
        assert_eq!(updates, vec![(id, "https://ex.com/from-extracted.jpg".into())]);
    }

    #[test]
    fn card_image_backfill_scan_includes_blank_image_url() {
        // A row whose `image_url` is blank (`''`, not NULL) must still be
        // scanned: `set_extracted_html`/`apply_card_images` treat NULL *or*
        // blank as missing, so a scan that only matched NULL would leave blank
        // rows permanently un-backfilled.
        let (conn, id) = test_db();
        conn.execute(
            "UPDATE articles
                SET image_url = '',
                    content_html = '<p>x</p><img src=\"https://ex.com/blank.jpg\">'
              WHERE id = ?1",
            [id],
        )
        .unwrap();

        let updates = card_image_backfill_scan(&conn).unwrap();
        assert_eq!(updates, vec![(id, "https://ex.com/blank.jpg".into())]);
    }

    /// Compact `NewHighlight` builder for the highlight tests.
    fn hl<'a>(
        article_id: i64,
        quote: &'a str,
        prefix: &'a str,
        suffix: &'a str,
        text_offset: i64,
        color: &'a str,
        note: &'a str,
    ) -> NewHighlight<'a> {
        NewHighlight {
            article_id,
            quote,
            prefix,
            suffix,
            text_offset,
            color,
            note,
        }
    }

    #[test]
    fn fresh_feed_has_no_last_fetched_until_touched() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        // A just-inserted feed has never been fetched.
        assert_eq!(feed_last_fetched(&conn, feed_id).unwrap(), None);
        // `touch_feed` (the same call `add_feed` makes after its initial
        // fetch) records the fetch time, so the feed no longer reads as
        // "never refreshed".
        touch_feed(&conn, feed_id).unwrap();
        assert!(feed_last_fetched(&conn, feed_id).unwrap().is_some());
    }

    #[test]
    fn refine_source_type_promotes_rss_but_never_demotes() {
        let (conn, _) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let kind = |c: &Connection| -> String {
            c.query_row("SELECT source_type FROM feeds WHERE id = ?1", params![feed_id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // The test feed starts generic.
        assert_eq!(kind(&conn), "rss");

        // A no-op when the refined kind is still `Rss`.
        refine_feed_source_type(&conn, feed_id, SourceType::Rss).unwrap();
        assert_eq!(kind(&conn), "rss");

        // A genuine kind promotes the still-generic feed.
        refine_feed_source_type(&conn, feed_id, SourceType::Podcast).unwrap();
        assert_eq!(kind(&conn), "podcast");

        // Once classified, a later call must not churn the type — the
        // `WHERE source_type = 'rss'` guard makes this strictly a promotion.
        refine_feed_source_type(&conn, feed_id, SourceType::Mastodon).unwrap();
        assert_eq!(kind(&conn), "podcast");
    }

    #[test]
    fn opml_export_omits_newsletter_sources() {
        use crate::ingestion::newsletter::NewsletterConfig;
        let (conn, _) = test_db();
        // The RSS feed from `test_db` plus a newsletter source whose feed_url
        // is the synthetic, non-HTTP-fetchable `imap://` form.
        let cfg = NewsletterConfig {
            host: "imap.example.com".into(),
            port: 993,
            username: "me@example.com".into(),
            password: "secret".into(),
            folder: "Newsletters".into(),
        };
        insert_newsletter_source(
            &conn,
            "imap://me@example.com@imap.example.com:993/Newsletters",
            "My Newsletter",
            &cfg,
        )
        .unwrap();

        let exported = feeds_for_export(&conn).unwrap();
        // Only the real RSS feed is exportable — the newsletter is left out so
        // a re-import never resurrects it as a broken `imap://` RSS feed.
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].1, "https://example.com/feed.xml");
        assert!(
            !exported.iter().any(|(_, url, _)| url.starts_with("imap://")),
            "no synthetic imap:// url should reach the OPML"
        );
    }

    #[test]
    fn insert_and_list_highlight() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "quoted text", "pre", "suf", 12, "yellow", ""))
            .unwrap();
        let all = list_highlights(&conn, aid).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, id);
        assert_eq!(all[0].quote, "quoted text");
        assert_eq!(all[0].prefix, "pre");
        assert_eq!(all[0].suffix, "suf");
        assert_eq!(all[0].text_offset, 12);
        assert_eq!(all[0].color, "yellow");
        assert_eq!(all[0].note, "");
    }

    #[test]
    fn highlights_ordered_by_offset() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "third", "", "", 90, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "first", "", "", 10, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "second", "", "", 50, "yellow", "")).unwrap();
        let quotes: Vec<String> = list_highlights(&conn, aid)
            .unwrap()
            .into_iter()
            .map(|h| h.quote)
            .collect();
        assert_eq!(quotes, ["first", "second", "third"]);
    }

    #[test]
    fn update_note_and_color() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        update_highlight_note(&conn, id, "a thought").unwrap();
        set_highlight_color(&conn, id, "green").unwrap();
        let h = get_highlight(&conn, id).unwrap().unwrap();
        assert_eq!(h.note, "a thought");
        assert_eq!(h.color, "green");
    }

    #[test]
    fn delete_highlight_removes_it() {
        let (conn, aid) = test_db();
        let id = insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        delete_highlight(&conn, id).unwrap();
        assert!(list_highlights(&conn, aid).unwrap().is_empty());
        assert!(get_highlight(&conn, id).unwrap().is_none());
    }

    #[test]
    fn highlights_cascade_on_article_delete() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "q", "", "", 0, "yellow", "")).unwrap();
        conn.execute("DELETE FROM articles WHERE id = ?1", params![aid])
            .unwrap();
        assert!(list_highlights(&conn, aid).unwrap().is_empty());
    }

    #[test]
    fn list_all_highlights_spans_articles() {
        let (conn, aid) = test_db();
        insert_highlight(&conn, &hl(aid, "one", "", "", 0, "yellow", "")).unwrap();
        insert_highlight(&conn, &hl(aid, "two", "", "", 5, "green", "noted")).unwrap();
        assert_eq!(list_all_highlights(&conn).unwrap().len(), 2);
    }

    // ── FTS query building ───────────────────────────────────────────

    #[test]
    fn fts_query_and_joins_explicit_search_terms() {
        // Explicit search: every word required (implicit FTS5 AND).
        assert_eq!(fts_query("rust async", false), "\"rust\"* \"async\"*");
    }

    #[test]
    fn fts_query_or_joins_for_recall() {
        // RAG retrieval: any word may match.
        assert_eq!(
            fts_query("rust async runtime", true),
            "\"rust\"* OR \"async\"* OR \"runtime\"*"
        );
    }

    #[test]
    fn fts_query_strips_punctuation_and_handles_empty() {
        // Non-alphanumerics are dropped from each term; an all-punctuation
        // input collapses to a match-nothing expression in both modes.
        assert_eq!(fts_query("c++!", false), "\"c\"*");
        assert_eq!(fts_query("!!! ???", true), "\"\"");
        assert_eq!(fts_query("   ", false), "\"\"");
    }

    #[test]
    fn fts_query_splits_punctuation_into_separate_terms() {
        // Punctuation *inside* a word splits it into separate terms, matching
        // how the unicode61 tokenizer indexes the article text — collapsing
        // `rust-lang` to `rustlang` would match nothing the index holds.
        assert_eq!(fts_query("rust-lang", false), "\"rust\"* \"lang\"*");
        assert_eq!(fts_query("node.js", true), "\"node\"* OR \"js\"*");
        assert_eq!(
            fts_query("co-op runtime", false),
            "\"co\"* \"op\"* \"runtime\"*"
        );
    }

    /// Insert a second article with searchable text for the RAG tests.
    fn add_article(conn: &Connection, feed_id: i64, guid: &str, title: &str, body: &str) {
        let article = NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: Some(format!("<p>{body}</p>")),
            body_text: body.into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &article, false, &[]).unwrap();
    }

    #[test]
    fn rag_search_matches_any_keyword_not_all() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        add_article(&conn, feed_id, "rust", "Rust news", "the borrow checker explained");
        add_article(&conn, feed_id, "privacy", "Privacy law", "a new data privacy regulation");

        // A natural-language question shares only *some* words with each
        // article. An AND join would require every word to appear and return
        // nothing; the OR-based RAG search still finds both relevant pieces.
        let hits = search_articles_for_rag(
            &conn,
            "what does the new privacy regulation say about the borrow checker",
            6,
        )
        .unwrap();
        let titles: Vec<&str> = hits.iter().map(|(_, t, _)| t.as_str()).collect();
        assert!(titles.contains(&"Rust news"), "got: {titles:?}");
        assert!(titles.contains(&"Privacy law"), "got: {titles:?}");
    }

    #[test]
    fn rag_search_empty_question_returns_no_rows() {
        let (conn, _aid) = test_db();
        // An all-stopword / punctuation-only question must not error and must
        // return nothing (the match-nothing `""` expression).
        assert!(search_articles_for_rag(&conn, "??? !!!", 6).unwrap().is_empty());
    }

    #[test]
    fn create_tag_is_idempotent_on_name() {
        let (conn, _aid) = test_db();
        let first = create_tag(&conn, "Rust").unwrap();
        // Re-creating the same name returns the existing id, not a constraint
        // error, and does not add a second row.
        let again = create_tag(&conn, "Rust").unwrap();
        assert_eq!(first, again);
        // Case-insensitive: "rust" resolves to the same tag as "Rust".
        let cased = create_tag(&conn, "rust").unwrap();
        assert_eq!(first, cased);
        assert_eq!(list_tags(&conn).unwrap().len(), 1);
    }

    #[test]
    fn create_tag_trims_whitespace_and_dedups_padded_names() {
        // A tag name with surrounding whitespace must resolve to the same tag
        // as its trimmed form — otherwise the `COLLATE NOCASE` lookup misses
        // and a visually identical near-duplicate tag is created.
        let (conn, _aid) = test_db();
        let rust = create_tag(&conn, "Rust").unwrap();
        assert_eq!(create_tag(&conn, "  Rust  ").unwrap(), rust);
        assert_eq!(create_tag(&conn, "\tRust\n").unwrap(), rust);
        // The stored name is the trimmed form.
        let go = create_tag(&conn, "  Go ").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM tags WHERE id = ?1", [go], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "Go");
        assert_eq!(list_tags(&conn).unwrap().len(), 2);
    }

    #[test]
    fn rename_tag_rejects_whitespace_padded_collision() {
        // A rename to a whitespace-padded variant of another tag's name must
        // still be rejected — the trim lets the clash check see through it.
        let (conn, _aid) = test_db();
        let _rust = create_tag(&conn, "Rust").unwrap();
        let go = create_tag(&conn, "Go").unwrap();
        let err = rename_tag(&conn, go, "  Rust  ").unwrap_err();
        assert!(matches!(err, AppError::Coded("tagNameExists")));
    }

    #[test]
    fn create_tag_after_middle_delete_sorts_last() {
        // Deleting a tag from the middle of the list leaves a gap in the
        // `position` sequence. A new tag must still land at the end — a
        // `COUNT(*)`-based position would collide with an existing row.
        let (conn, _aid) = test_db();
        let a = create_tag(&conn, "alpha").unwrap();
        let b = create_tag(&conn, "beta").unwrap();
        let _c = create_tag(&conn, "gamma").unwrap();
        delete_tag(&conn, b).unwrap();
        let zoo = create_tag(&conn, "zeta").unwrap();

        let order: Vec<i64> = list_tags(&conn).unwrap().iter().map(|t| t.id).collect();
        assert_eq!(
            order.last(),
            Some(&zoo),
            "a tag created after a middle delete must sort last, got {order:?}",
        );
        // The pre-existing tags keep their relative order.
        assert!(
            order.iter().position(|&x| x == a) < order.iter().position(|&x| x == zoo),
        );
    }

    #[test]
    fn create_rule_after_middle_delete_sorts_last() {
        // Same `COUNT(*)`-vs-`MAX(position)` hazard as tags: deleting a rule
        // from the middle leaves a `position` gap, so a `COUNT(*)`-based
        // position collides with an existing row and the new rule no longer
        // sorts last. Rule order is load-bearing for ingestion evaluation.
        let (conn, _aid) = test_db();
        // Five rules, positions {0,1,2,3,4}.
        let _a = create_rule(&conn, "alpha", None, "title", "x", "skip").unwrap();
        let b = create_rule(&conn, "beta", None, "title", "y", "skip").unwrap();
        let c = create_rule(&conn, "gamma", None, "title", "z", "skip").unwrap();
        let _d = create_rule(&conn, "delta", None, "title", "w", "skip").unwrap();
        let _e = create_rule(&conn, "epsilon", None, "title", "u", "skip").unwrap();
        // Delete two from the middle, leaving positions {0,3,4} — wide enough
        // that a `COUNT(*)` value (3) collides with a non-last rule.
        delete_rule(&conn, b).unwrap();
        delete_rule(&conn, c).unwrap();
        let zoo = create_rule(&conn, "zeta", None, "title", "v", "skip").unwrap();

        let order: Vec<i64> = list_rules(&conn).unwrap().iter().map(|r| r.id).collect();
        assert_eq!(
            order.last(),
            Some(&zoo),
            "a rule created after a middle delete must sort last, got {order:?}",
        );
    }

    // ── per-feed unread count ────────────────────────────────────────

    #[test]
    fn count_feed_unread_excludes_articles_pre_marked_read_by_a_rule() {
        // A filter rule with a `read` action inserts a matching article
        // already marked read. `count_feed_unread` must agree with
        // `list_feeds`: it counts only genuinely-unread rows.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        // The fixture article is unread.
        assert_eq!(count_feed_unread(&conn, feed_id).unwrap(), 1);

        // A rule that pre-marks anything titled "Sponsored" as read.
        create_rule(&conn, "ads", None, "title", "Sponsored", "read").unwrap();
        let rules = active_rules(&conn).unwrap();

        let read_by_rule = NewArticle {
            guid: "g-sponsored".into(),
            url: Some("https://example.com/sponsored".into()),
            title: "Sponsored Post".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "ad copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        let plain = NewArticle {
            guid: "g-plain".into(),
            url: Some("https://example.com/plain".into()),
            title: "A Normal Post".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "ordinary copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        // Both rows land, but `upsert_article` returns `true` only for the
        // genuinely-unread one — the rule-read article is not "new".
        assert!(
            !upsert_article(&conn, feed_id, &read_by_rule, false, &rules).unwrap(),
            "an article pre-marked read by a rule is not a new unread article"
        );
        assert!(upsert_article(&conn, feed_id, &plain, false, &rules).unwrap());

        // Three articles inserted total, but the rule-read one is not unread.
        assert_eq!(
            count_feed_unread(&conn, feed_id).unwrap(),
            2,
            "the rule-read article must not be counted as unread"
        );
        // And it matches the count `list_feeds` computes for the same feed.
        let from_list = list_feeds(&conn)
            .unwrap()
            .into_iter()
            .find(|f| f.id == feed_id)
            .unwrap()
            .unread_count;
        assert_eq!(from_list, 2);
    }

    #[test]
    fn upsert_article_reports_rule_read_inserts_as_not_new() {
        // The refresh scheduler tallies `upsert_article(..) == Ok(true)` into
        // the "N new articles" count that drives the refresh toast and the OS
        // notification. An article inserted but pre-marked read by a `read`
        // rule never appears as unread, so it must NOT be counted as new —
        // otherwise the toast/notification claims new articles the user can
        // never find.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        create_rule(&conn, "ads", None, "title", "Sponsored", "read").unwrap();
        let rules = active_rules(&conn).unwrap();

        let mk = |guid: &str, title: &str| NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "copy".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };

        // Pre-marked read by the rule → not new.
        assert!(!upsert_article(&conn, feed_id, &mk("g-ad", "Sponsored Item"), false, &rules)
            .unwrap());
        // A plain article → genuinely new.
        assert!(upsert_article(&conn, feed_id, &mk("g-ok", "Real Story"), false, &rules)
            .unwrap());
        // A duplicate guid → not new (no double count).
        assert!(!upsert_article(&conn, feed_id, &mk("g-ok", "Real Story"), false, &rules)
            .unwrap());
    }

    // ── rule matching ────────────────────────────────────────────────

    #[test]
    fn any_field_rule_does_not_match_keyword_across_field_boundary() {
        // An `any`-field rule must check each field independently — the same
        // per-column semantics `preview_rule` uses. A keyword that only exists
        // because the title's tail and the body's head happen to abut must NOT
        // fire the rule; otherwise live ingestion acts on articles the rule
        // preview never counted.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();

        // A `skip` rule keyed on the two-word phrase "rust weekly".
        create_rule(&conn, "rw", None, "any", "rust weekly", "skip").unwrap();
        let rules = active_rules(&conn).unwrap();

        // Title ends in "rust", author starts with "weekly": the old code
        // concatenated `title author body` with single spaces, so the phrase
        // appeared only at that join. Per-field matching must not fire here,
        // and the article must still insert.
        let straddle = NewArticle {
            guid: "g-straddle".into(),
            url: Some("https://example.com/straddle".into()),
            title: "All about rust".into(),
            author: Some("Weekly Digest".into()),
            summary: None,
            content_html: None,
            body_text: "body text".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            upsert_article(&conn, feed_id, &straddle, false, &rules).unwrap(),
            "a keyword straddling the title/author boundary must not skip the article"
        );

        // The phrase wholly within one field still triggers the skip.
        let within = NewArticle {
            guid: "g-within".into(),
            url: Some("https://example.com/within".into()),
            title: "The Rust Weekly roundup".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "intro text".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            !upsert_article(&conn, feed_id, &within, false, &rules).unwrap(),
            "a keyword wholly within one field must still skip the article"
        );
    }

    #[test]
    fn preview_rule_case_folds_non_ascii_like_live_ingestion() {
        // `rule_matches` folds case with Rust's Unicode-aware `to_lowercase()`,
        // so a `café` rule matches a `CAFÉ` article during ingestion. SQLite's
        // built-in `LOWER()` is ASCII-only and would leave `É` uppercase,
        // making the preview undercount — `preview_rule` must use the
        // Unicode-aware `unicode_lower` so its count agrees with ingestion.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();

        // An article whose title carries an upper-case non-ASCII letter.
        let article = NewArticle {
            guid: "g-cafe".into(),
            url: Some("https://example.com/cafe".into()),
            title: "CAFÉ CULTURE in Zürich".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(upsert_article(&conn, feed_id, &article, false, &[]).unwrap());

        // Lower-case non-ASCII keywords must still count the article.
        for keyword in ["café", "zürich"] {
            let (count, samples) =
                preview_rule(&conn, None, "title", keyword).unwrap();
            assert_eq!(count, 1, "keyword `{keyword}` should match the CAFÉ article");
            assert_eq!(samples.len(), 1);
        }

        // And the rule that the preview describes must agree at ingest time:
        // a `skip` rule on `café` drops a fresh `CAFÉ`-titled article.
        create_rule(&conn, "no-cafe", None, "title", "café", "skip").unwrap();
        let rules = active_rules(&conn).unwrap();
        let fresh = NewArticle {
            guid: "g-cafe-2".into(),
            url: Some("https://example.com/cafe2".into()),
            title: "Another CAFÉ Story".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        assert!(
            !upsert_article(&conn, feed_id, &fresh, false, &rules).unwrap(),
            "ingestion must skip the article the `café` preview counted"
        );
    }

    // ── apply_rule_to_existing (retroactive backfill on save) ─────────

    fn seed(conn: &Connection, feed_id: i64, guid: &str, title: &str) {
        let a = NewArticle {
            guid: guid.into(),
            url: Some(format!("https://example.com/{guid}")),
            title: title.into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: "body".into(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &a, false, &[]).unwrap();
    }

    #[test]
    fn apply_rule_to_existing_stars_matching_backlog_idempotently() {
        // The bug this guards: saving a `star` rule did nothing to articles
        // already stored — only `upsert_article` ran the rule, and that fires
        // only on freshly fetched articles. `apply_rule_to_existing` backfills
        // the matches when the rule is saved.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        seed(&conn, feed_id, "j1", "Learning Java today");
        seed(&conn, feed_id, "j2", "JavaScript tips");
        seed(&conn, feed_id, "p1", "Rust ownership");

        let n = apply_rule_to_existing(&conn, None, "title", "java", "star").unwrap();
        assert_eq!(n, 2, "both Java/JavaScript titles get starred");
        let starred: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles WHERE is_starred = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(starred, 2);

        // Re-running counts only rows it actually changes — already-starred rows
        // are excluded, so a second save reports 0.
        let again = apply_rule_to_existing(&conn, None, "title", "java", "star").unwrap();
        assert_eq!(again, 0);
    }

    #[test]
    fn apply_rule_to_existing_skip_deletes_matches_keeping_fts_in_sync() {
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        seed(&conn, feed_id, "a1", "Sponsored junk");
        seed(&conn, feed_id, "a2", "more Sponsored stuff");
        seed(&conn, feed_id, "a3", "real content");

        // The preview count is exactly the set the skip apply deletes — they
        // share `rule_match_where`, so the user is never surprised.
        let (preview, _) = preview_rule(&conn, None, "title", "sponsored").unwrap();
        assert_eq!(preview, 2);
        let n = apply_rule_to_existing(&conn, None, "title", "sponsored", "skip").unwrap();
        assert_eq!(n, 2);

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE title LIKE '%Sponsored%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "matching articles are deleted");

        // The `articles_fts_ad` trigger drops the FTS rows with the articles, so
        // the index never goes stale.
        let arts: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles", [], |r| r.get(0))
            .unwrap();
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM articles_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(arts, fts, "FTS index stays in sync after a skip-rule delete");
    }

    #[test]
    fn apply_rule_to_existing_scopes_to_one_feed() {
        let (conn, _aid) = test_db();
        let feed_a: i64 = conn
            .query_row("SELECT feed_id FROM articles LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let feed_b = insert_feed(
            &conn,
            "https://example.com/b.xml",
            None,
            "Feed B",
            None,
            SourceType::Rss,
            None,
        )
        .unwrap();
        seed(&conn, feed_a, "x1", "Deals everywhere");
        seed(&conn, feed_b, "x2", "Deals galore");

        // Scoped to feed B only: feed A's match is left untouched.
        let n = apply_rule_to_existing(&conn, Some(feed_b), "title", "deals", "star").unwrap();
        assert_eq!(n, 1);
        let starred_in_a: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM articles WHERE is_starred = 1 AND feed_id = ?1",
                params![feed_a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(starred_in_a, 0, "a feed-scoped rule must not touch other feeds");
    }

    // ── mark_all_read sync queueing ──────────────────────────────────

    #[test]
    fn mark_all_read_queues_articles_without_a_remote_id() {
        // Freshly fetched articles carry no `remote_id` until a sync pull
        // matches them by URL. A bulk "mark all read" run right after a
        // refresh must still queue those changes so they reach the sync
        // server once the id is assigned — not silently drop them.
        let (conn, aid) = test_db();
        assert_eq!(
            conn.query_row("SELECT remote_id FROM articles WHERE id = ?1", [aid], |r| r
                .get::<_, Option<String>>(0))
                .unwrap(),
            None,
            "fixture article should start without a remote id"
        );

        let n = mark_all_read(&conn, &ArticleQuery::All, true).unwrap();
        assert_eq!(n, 1, "the one unread article should be flipped to read");

        let queued: i64 = conn
            .query_row(
                "SELECT count(*) FROM sync_queue WHERE article_id = ?1 AND field = 'read'",
                [aid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queued, 1, "the read change must be queued for sync");
    }

    #[test]
    fn mark_all_read_skips_sync_queue_when_not_connected() {
        // Without a sync server linked, no rows should land in the queue.
        let (conn, _aid) = test_db();
        mark_all_read(&conn, &ArticleQuery::All, false).unwrap();
        let queued: i64 = conn
            .query_row("SELECT count(*) FROM sync_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queued, 0);
    }

    // ── retention cleanup ────────────────────────────────────────────

    /// Insert a read article with an explicit RFC 3339 `published_at`, the
    /// format `to_rfc3339` produces for every feed-dated article.
    fn insert_read_article_published(conn: &Connection, feed_id: i64, guid: &str, rfc3339: &str) {
        let a = NewArticle {
            guid: guid.into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: Some(rfc3339.into()),
            enclosures: Vec::new(),
        };
        upsert_article(conn, feed_id, &a, false, &[]).unwrap();
        conn.execute(
            "UPDATE articles SET is_read = 1 WHERE guid = ?1",
            params![guid],
        )
        .unwrap();
    }

    #[test]
    fn cleanup_compares_rfc3339_published_at_by_real_instant() {
        // `published_at` is stored RFC 3339 (`...T...+00:00`) while the
        // retention cutoff uses SQLite's space-separated form. A raw string
        // `<` mis-orders the two: the `T` byte sorts *after* a space, so for
        // an article whose `published_at` falls on the same calendar day as
        // the cutoff but earlier in the day, the string compare wrongly
        // reports it as newer and it escapes deletion. The fix normalises
        // both sides with `datetime()`.
        //
        // This test pins exactly that same-day boundary: an article dated to
        // the cutoff's own calendar day but earlier in that day — genuinely
        // outside a 30-day window, yet a string compare wrongly keeps it.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();

        // The cutoff is "now" minus 30 days, kept at the current wall-clock
        // time. Date this article to that very same calendar day but at one
        // second past midnight — genuinely older than the cutoff instant,
        // yet on a string `<` the RFC 3339 `T` makes it look newer.
        let now = chrono::Utc::now();
        let cutoff_day = (now - chrono::Duration::days(30)).date_naive();
        let old = cutoff_day.and_hms_opt(0, 0, 1).unwrap().and_utc();
        // Skip the rare case where the test runs within a second of midnight
        // and the article is not actually before the cutoff.
        assert!(old < now - chrono::Duration::days(30));
        insert_read_article_published(&conn, feed_id, "old", &old.to_rfc3339());
        // Comfortably inside the window — must be kept.
        let recent = chrono::Utc::now() - chrono::Duration::days(1);
        insert_read_article_published(&conn, feed_id, "recent", &recent.to_rfc3339());

        let removed = cleanup_old_articles(&conn, 30).unwrap();
        assert_eq!(removed, 1, "exactly the past-cutoff article should go");

        let surviving: Vec<String> = conn
            .prepare("SELECT guid FROM articles WHERE guid IN ('old','recent')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(surviving, ["recent"], "the old article must be deleted");
    }

    #[test]
    fn cleanup_keeps_starred_and_read_later_articles() {
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_id, "starred", &old);
        insert_read_article_published(&conn, feed_id, "later", &old);
        conn.execute("UPDATE articles SET is_starred = 1 WHERE guid = 'starred'", [])
            .unwrap();
        conn.execute("UPDATE articles SET read_later = 1 WHERE guid = 'later'", [])
            .unwrap();

        cleanup_old_articles(&conn, 30).unwrap();
        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM articles WHERE guid IN ('starred','later')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 2, "starred / read-later articles are never purged");
    }

    #[test]
    fn cleanup_keeps_highlighted_articles() {
        // A read article the user has highlighted must survive retention: the
        // highlights cascade-delete with the article, so purging it would
        // silently destroy the user's annotations. An unhighlighted read
        // article of the same age is still purged.
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let old = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339();
        insert_read_article_published(&conn, feed_id, "annotated", &old);
        insert_read_article_published(&conn, feed_id, "plain", &old);

        let annotated_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'annotated'", [], |r| {
                r.get(0)
            })
            .unwrap();
        insert_highlight(
            &conn,
            &hl(annotated_id, "kept quote", "", "", 0, "yellow", ""),
        )
        .unwrap();

        let removed = cleanup_old_articles(&conn, 30).unwrap();
        assert_eq!(removed, 1, "only the unhighlighted read article is purged");

        let surviving: Vec<String> = conn
            .prepare("SELECT guid FROM articles WHERE guid IN ('annotated','plain')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(surviving, ["annotated"], "the highlighted article is kept");
        // And its highlights are intact, not cascade-deleted.
        assert_eq!(list_highlights(&conn, annotated_id).unwrap().len(), 1);
    }

    #[test]
    fn cleanup_with_non_positive_days_is_a_no_op() {
        // A retention window of 0 days builds the modifier `'-0 days'`, whose
        // cutoff `datetime('now', '-0 days')` is *now* — left unguarded the
        // DELETE would purge every read article regardless of age. A negative
        // window builds a malformed `'--N days'` modifier. Both must be
        // rejected as no-ops so a bad caller / corrupt setting cannot trigger
        // a mass deletion.
        let (conn, _fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        // A read article published just now — well inside any sane window,
        // yet `datetime('now', '-0 days')` would still sweep it.
        let now = chrono::Utc::now().to_rfc3339();
        insert_read_article_published(&conn, feed_id, "fresh", &now);

        assert_eq!(cleanup_old_articles(&conn, 0).unwrap(), 0, "0 days deletes nothing");
        assert_eq!(cleanup_old_articles(&conn, -30).unwrap(), 0, "negative days deletes nothing");

        let kept: i64 = conn
            .query_row(
                "SELECT count(*) FROM articles WHERE guid = 'fresh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 1, "the fresh article survives a non-positive window");
    }

    // ── article-list chronological ordering ──────────────────────────

    #[test]
    fn list_articles_orders_mixed_date_formats_by_real_instant() {
        // The newest-first list sorts on COALESCE(published_at, fetched_at).
        // A feed-dated row carries `published_at` as RFC 3339 (`...T...+00:00`,
        // a `T` separator); a dateless row falls through to `fetched_at`, which
        // SQLite stores space-separated (`... ...`). A raw string `<` compares
        // the two formats byte-for-byte and the `T` (0x54) sorts *after* a
        // space (0x20), so a dated row looks up to a day newer than it is —
        // a list mixing both kinds of rows comes out subtly out of order.
        //
        // This pins that exact mix: a dateless row fetched *later* than a
        // dated row was published. The dateless one must sort first. Under a
        // string compare the dated row wins (the `T`); `datetime()` fixes it.
        let (conn, fixture) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT feed_id FROM articles WHERE id = ?1", [fixture], |r| {
                r.get(0)
            })
            .unwrap();
        // Drop the bare fixture article so only the two controlled rows remain.
        conn.execute("DELETE FROM articles WHERE id = ?1", [fixture])
            .unwrap();

        // Same calendar day so the format difference — not the date — decides:
        // the dated row published at 10:00, the dateless row fetched at 12:00.
        // '2024-01-15T10:00:00+00:00' > '2024-01-15 12:00:00' as raw strings
        // (T beats space) but the dateless row is the genuinely newer one.
        insert_read_article_published(&conn, feed_id, "dated", "2024-01-15T10:00:00+00:00");
        let dateless = NewArticle {
            guid: "dateless".into(),
            url: None,
            title: "T".into(),
            author: None,
            summary: None,
            content_html: None,
            body_text: String::new(),
            image_url: None,
            published_at: None,
            enclosures: Vec::new(),
        };
        upsert_article(&conn, feed_id, &dateless, false, &[]).unwrap();
        conn.execute(
            "UPDATE articles SET fetched_at = '2024-01-15 12:00:00' WHERE guid = 'dateless'",
            [],
        )
        .unwrap();

        // Newest-first: the dateless row (fetched 12:00) precedes the dated
        // row (published 10:00).
        let newest = list_articles(&conn, &ArticleQuery::All, false, None, false, 50, 0).unwrap();
        let newest_guids: Vec<i64> = newest.iter().map(|a| a.id).collect();
        let dated_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'dated'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let dateless_id: i64 = conn
            .query_row("SELECT id FROM articles WHERE guid = 'dateless'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            newest_guids,
            [dateless_id, dated_id],
            "newest-first: the later-fetched dateless article must come first"
        );

        // Oldest-first is the exact mirror.
        let oldest = list_articles(&conn, &ArticleQuery::All, false, None, true, 50, 0).unwrap();
        let oldest_guids: Vec<i64> = oldest.iter().map(|a| a.id).collect();
        assert_eq!(
            oldest_guids,
            [dated_id, dateless_id],
            "oldest-first: the earlier-published dated article must come first"
        );
    }

    // ── feed rename vs. refresh ──────────────────────────────────────

    #[test]
    fn manual_rename_survives_a_metadata_refresh() {
        // A user renames a feed; a later refresh pulls the feed document's own
        // `<title>` through `update_feed_meta`. The rename must stick — only
        // the other metadata (site_url, description, favicon) should update.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        rename_feed(&conn, feed_id, "My Custom Name").unwrap();

        // Simulate a refresh: the feed document still calls itself "Example Feed".
        update_feed_meta(
            &conn,
            feed_id,
            Some("Example Feed"),
            Some("https://example.com"),
            Some("A description"),
            None,
        )
        .unwrap();

        let (title, site_url): (String, Option<String>) = conn
            .query_row("SELECT title, site_url FROM feeds WHERE id = ?1", [feed_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            title, "My Custom Name",
            "a manual rename must not be reverted by a refresh"
        );
        assert_eq!(
            site_url.as_deref(),
            Some("https://example.com"),
            "non-title metadata must still refresh normally"
        );
    }

    #[test]
    fn rename_feed_rejects_an_empty_title() {
        // An empty (or whitespace-only) rename must be refused: a rename sets
        // `custom_title = 1`, so an empty title would blank the feed in the
        // sidebar forever — `update_feed_meta` can no longer restore it. The
        // original title must be left untouched.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        let original: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();

        for blank in ["", "   ", "\t\n"] {
            let err = rename_feed(&conn, feed_id, blank).unwrap_err();
            assert!(
                err.to_string().contains("emptyFeedTitle"),
                "blank rename {blank:?} should be rejected, got: {err}"
            );
        }

        let (title, custom): (String, bool) = conn
            .query_row(
                "SELECT title, custom_title FROM feeds WHERE id = ?1",
                [feed_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(title, original, "a rejected rename must not alter the title");
        assert!(!custom, "a rejected rename must not set the custom_title flag");
    }

    #[test]
    fn rename_feed_trims_surrounding_whitespace() {
        // A padded name (`"  News  "`) is stored trimmed — the db function is
        // the single trimming chokepoint, so the command no longer trims.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();
        rename_feed(&conn, feed_id, "  Tech News  ").unwrap();
        let title: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Tech News");
    }

    #[test]
    fn refresh_updates_title_when_not_renamed() {
        // A feed the user has never renamed should still pick up the feed
        // document's title on refresh — the guard only protects manual names.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        update_feed_meta(&conn, feed_id, Some("Renamed Upstream"), None, None, None).unwrap();

        let title: String = conn
            .query_row("SELECT title FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))
            .unwrap();
        assert_eq!(title, "Renamed Upstream");
    }

    #[test]
    fn refresh_with_empty_title_keeps_existing_name() {
        // `feed-rs` parses `<title></title>` (or a stray empty title) as
        // `Some("")`, and the scheduler's refresh path forwards it straight
        // into `update_feed_meta`. An empty string must be treated like
        // `None` — the feed's good sidebar name must survive, not be wiped
        // blank. The other metadata columns get the same empty-string guard.
        let (conn, _aid) = test_db();
        let feed_id: i64 = conn
            .query_row("SELECT id FROM feeds", [], |r| r.get(0))
            .unwrap();

        // Seed real metadata first (the feed has never been renamed by hand).
        update_feed_meta(
            &conn,
            feed_id,
            Some("Good Feed Name"),
            Some("https://example.com"),
            Some("A description"),
            Some("https://example.com/favicon.ico"),
        )
        .unwrap();

        // A later refresh serves empty metadata — every field must be ignored.
        update_feed_meta(
            &conn,
            feed_id,
            Some(""),
            Some(""),
            Some(""),
            Some(""),
        )
        .unwrap();

        let (title, site_url, description, favicon): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT title, site_url, description, favicon_url FROM feeds WHERE id = ?1",
                [feed_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            title, "Good Feed Name",
            "an empty <title> on refresh must not blank the sidebar name"
        );
        assert_eq!(site_url.as_deref(), Some("https://example.com"));
        assert_eq!(description.as_deref(), Some("A description"));
        assert_eq!(favicon.as_deref(), Some("https://example.com/favicon.ico"));
    }

    #[test]
    fn rename_tag_rejects_collision_with_another_tag() {
        let (conn, _aid) = test_db();
        let rust = create_tag(&conn, "Rust").unwrap();
        let go = create_tag(&conn, "Go").unwrap();

        // Renaming "Go" onto an exact match of "Rust" must be rejected with the
        // localisable code rather than the raw UNIQUE-constraint SQLite error.
        let err = rename_tag(&conn, go, "Rust").unwrap_err();
        assert!(
            matches!(err, AppError::Coded("tagNameExists")),
            "expected tagNameExists, got {err:?}"
        );
        // A *case variant* of another tag must be rejected too — otherwise it
        // would create the near-duplicate `create_tag` deliberately collapses.
        let err = rename_tag(&conn, go, "rust").unwrap_err();
        assert!(matches!(err, AppError::Coded("tagNameExists")));

        // The clash check did not corrupt either name.
        let name = |id| {
            conn.query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(name(rust), "Rust");
        assert_eq!(name(go), "Go");
    }

    #[test]
    fn rename_tag_allows_genuine_rename_and_self_case_change() {
        let (conn, _aid) = test_db();
        let id = create_tag(&conn, "draft").unwrap();
        // A free name is accepted.
        rename_tag(&conn, id, "Reading").unwrap();
        // Re-casing the tag's *own* name is allowed (no other tag clashes).
        rename_tag(&conn, id, "READING").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM tags WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "READING");
    }

    // --- folders: same name-uniqueness family as tags. `folders.name` has no
    //     UNIQUE constraint, so create/rename must dedup in code. ---

    #[test]
    fn create_folder_is_idempotent_on_name() {
        let (conn, _aid) = test_db();
        let first = create_folder(&conn, "Tech").unwrap();
        // Re-creating the same name returns the existing id, not a second row.
        assert_eq!(create_folder(&conn, "Tech").unwrap(), first);
        // Case-insensitive: "tech" resolves to the same folder as "Tech".
        assert_eq!(create_folder(&conn, "tech").unwrap(), first);
        assert_eq!(list_folders(&conn).unwrap().len(), 1);
    }

    #[test]
    fn folder_id_by_name_reuses_existing_folder_case_insensitively() {
        // OPML import attaches feeds via `folder_id_by_name`; an imported
        // folder whose name matches an existing one (in any case) must reuse
        // that folder rather than spawn a near-duplicate.
        let (conn, _aid) = test_db();
        let existing = create_folder(&conn, "News").unwrap();
        assert_eq!(folder_id_by_name(&conn, "news").unwrap(), existing);
        assert_eq!(list_folders(&conn).unwrap().len(), 1);
        // A genuinely new name still creates a folder.
        let fresh = folder_id_by_name(&conn, "Science").unwrap();
        assert_ne!(fresh, existing);
        assert_eq!(list_folders(&conn).unwrap().len(), 2);
    }

    #[test]
    fn rename_folder_rejects_collision_with_another_folder() {
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        let news = create_folder(&conn, "News").unwrap();

        // Renaming "News" onto an exact match of "Tech" is rejected with the
        // localisable code.
        let err = rename_folder(&conn, news, "Tech").unwrap_err();
        assert!(
            matches!(err, AppError::Coded("folderNameExists")),
            "expected folderNameExists, got {err:?}"
        );
        // A case variant of another folder is rejected too.
        let err = rename_folder(&conn, news, "tech").unwrap_err();
        assert!(matches!(err, AppError::Coded("folderNameExists")));

        // Neither name was corrupted by the clash check.
        let name = |id| {
            conn.query_row("SELECT name FROM folders WHERE id = ?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .unwrap()
        };
        assert_eq!(name(tech), "Tech");
        assert_eq!(name(news), "News");
    }

    #[test]
    fn rename_folder_allows_genuine_rename_and_self_case_change() {
        let (conn, _aid) = test_db();
        let id = create_folder(&conn, "Misc").unwrap();
        // A free name is accepted.
        rename_folder(&conn, id, "Reading").unwrap();
        // Re-casing the folder's *own* name is allowed (no other folder clashes).
        rename_folder(&conn, id, "READING").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "READING");
    }

    #[test]
    fn create_folder_trims_whitespace_and_dedups_padded_names() {
        // An OPML-imported folder name carrying surrounding whitespace
        // (`<outline text=" Tech ">`) must resolve to the same folder as a
        // plain "Tech" — without the trim the `COLLATE NOCASE` lookup misses
        // it and a second, visually identical folder is spawned.
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        assert_eq!(create_folder(&conn, "  Tech  ").unwrap(), tech);
        assert_eq!(create_folder(&conn, "\tTech\n").unwrap(), tech);
        // The first creation also stores the trimmed form, not the padded one.
        let padded = create_folder(&conn, " News ").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [padded], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "News");
        assert_eq!(list_folders(&conn).unwrap().len(), 2);
    }

    #[test]
    fn rename_folder_rejects_whitespace_padded_collision() {
        // A rename to a whitespace-padded variant of another folder's name
        // must still be rejected — the trim makes the clash check see through
        // the padding instead of letting the near-duplicate slip past.
        let (conn, _aid) = test_db();
        let _tech = create_folder(&conn, "Tech").unwrap();
        let news = create_folder(&conn, "News").unwrap();
        let err = rename_folder(&conn, news, "  Tech  ").unwrap_err();
        assert!(matches!(err, AppError::Coded("folderNameExists")));
    }

    #[test]
    fn create_folder_rejects_an_empty_or_blank_name() {
        // An empty / whitespace-only name must be refused at the DB chokepoint:
        // `import_opml` reaches `create_folder` through `folder_id_by_name`
        // without the `PromptDialog` guard, so a blank label would otherwise
        // insert an unlabelled folder into the sidebar. No row may be created.
        let (conn, _aid) = test_db();
        for blank in ["", "   ", "\t\n"] {
            let err = create_folder(&conn, blank).unwrap_err();
            assert!(
                matches!(err, AppError::Coded("emptyFolderName")),
                "blank name {blank:?} must be rejected"
            );
        }
        assert!(list_folders(&conn).unwrap().is_empty());
    }

    #[test]
    fn rename_folder_rejects_an_empty_or_blank_name() {
        // Renaming a folder to a blank string would leave it unlabelled with
        // no recovery path — rejected the same way `create_folder` is.
        let (conn, _aid) = test_db();
        let tech = create_folder(&conn, "Tech").unwrap();
        for blank in ["", "   ", "\t\n"] {
            let err = rename_folder(&conn, tech, blank).unwrap_err();
            assert!(matches!(err, AppError::Coded("emptyFolderName")));
        }
        // The folder keeps its original name.
        let name: String = conn
            .query_row("SELECT name FROM folders WHERE id = ?1", [tech], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Tech");
    }
}
