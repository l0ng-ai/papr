//! papr — an Agent eXperience Interface (AXI) over a local Papr RSS database.
//!
//! Reads, searches, triages and refreshes your feeds from the shell, emitting
//! token-efficient TOON on stdout. Designed to be driven by autonomous agents:
//! minimal default schemas, truncated long text with an escape hatch,
//! pre-computed aggregates, definitive empty states, idempotent mutations, and
//! structured errors (also on stdout). Diagnostics go to stderr.

mod toon;

use clap::{Parser, Subcommand};
use papr_core::db;
use papr_core::ingestion::{fetch, parse, refresh};
use papr_core::models::ArticleQuery;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use toon::{opt, scalar, Out};

const APP_IDENTIFIER: &str = "com.thomas.papr";
const DESCRIPTION: &str = "Read, search and triage your Papr RSS feeds from the shell.";
const USER_AGENT: &str = concat!("papr-cli/", env!("CARGO_PKG_VERSION"));

/// Body truncation budget for a single `read`, and the tighter one when reading
/// several articles at once (keeping a batch affordable in tokens).
const READ_TRUNCATE: usize = 1500;
const BATCH_TRUNCATE: usize = 600;
/// Default rows for `list` / `search`.
const LIST_LIMIT: i64 = 30;
const SEARCH_LIMIT: i64 = 20;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            print!("{}", render_error(&AxiError::runtime(format!("runtime: {e}"))));
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(body) => {
            print!("{body}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            print!("{}", render_error(&err));
            err.exit_code()
        }
    }
}

// ───────────────────────────── CLI surface ─────────────────────────────

#[derive(Parser)]
#[command(name = "papr", version, about = DESCRIPTION, disable_help_subcommand = true)]
struct Cli {
    /// Path to the Papr SQLite database (defaults to the desktop app's data dir;
    /// override with the PAPR_DB env var).
    #[arg(long, global = true, env = "PAPR_DB", value_name = "PATH")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List subscribed feeds, grouped by folder, with per-feed unread counts.
    Feeds,
    /// List articles. Defaults to unread; combine filters freely.
    List(ListArgs),
    /// Read one or more articles as plain text (truncated unless --full).
    Read(ReadArgs),
    /// Full-text search across every article (FTS5).
    Search {
        /// The search query.
        query: String,
        /// Maximum matches to return.
        #[arg(long, default_value_t = SEARCH_LIMIT)]
        limit: i64,
    },
    /// Change article state: read | unread | star | unstar | later | unlater.
    Mark {
        /// New state to apply.
        #[arg(value_parser = ["read", "unread", "star", "unstar", "later", "unlater"])]
        state: String,
        /// One or more article ids.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<i64>,
    },
    /// List tags with their article counts.
    Tags,
    /// Subscribe to a feed by URL (auto-discovers the feed, then fetches it).
    Subscribe {
        /// A feed URL, or a site URL to auto-discover a feed from.
        url: String,
        /// Place the new feed in this folder id.
        #[arg(long, value_name = "ID")]
        folder: Option<i64>,
    },
    /// Fetch new articles over the network (RSS feeds and newsletter mailboxes).
    Refresh {
        /// Only refresh this feed id (default: all feeds).
        #[arg(long, value_name = "ID")]
        feed: Option<i64>,
    },
}

#[derive(clap::Args)]
struct ListArgs {
    /// Only this feed id.
    #[arg(long, value_name = "ID")]
    feed: Option<i64>,
    /// Only feeds in this folder id.
    #[arg(long, value_name = "ID")]
    folder: Option<i64>,
    /// Only articles carrying this tag id.
    #[arg(long, value_name = "ID")]
    tag: Option<i64>,
    /// Only starred articles.
    #[arg(long)]
    starred: bool,
    /// Only read-later articles.
    #[arg(long)]
    later: bool,
    /// Include already-read articles (default shows unread only).
    #[arg(long)]
    all: bool,
    /// Maximum rows to return.
    #[arg(long, default_value_t = LIST_LIMIT)]
    limit: i64,
    /// Skip this many rows (pagination).
    #[arg(long, default_value_t = 0)]
    offset: i64,
}

#[derive(clap::Args)]
struct ReadArgs {
    /// Article ids to read. Omit to read by filter (--feed/--tag/...).
    ids: Vec<i64>,
    /// Show the complete body instead of a truncated preview.
    #[arg(long)]
    full: bool,
    /// Read the latest articles of this feed id.
    #[arg(long, value_name = "ID")]
    feed: Option<i64>,
    /// Read the latest articles in this folder id.
    #[arg(long, value_name = "ID")]
    folder: Option<i64>,
    /// Read the latest articles carrying this tag id.
    #[arg(long, value_name = "ID")]
    tag: Option<i64>,
    /// With a filter, restrict to unread articles.
    #[arg(long)]
    unread: bool,
    /// With a filter, how many articles to read.
    #[arg(long, default_value_t = 5)]
    limit: i64,
}

// ───────────────────────────── dispatch ─────────────────────────────

async fn run(cli: Cli) -> Result<String, AxiError> {
    let path = db_path(&cli)?;
    match cli.cmd {
        None => cmd_home(&path),
        Some(Cmd::Feeds) => cmd_feeds(&path),
        Some(Cmd::List(args)) => cmd_list(&path, args),
        Some(Cmd::Read(args)) => cmd_read(&path, args),
        Some(Cmd::Search { query, limit }) => cmd_search(&path, &query, limit),
        Some(Cmd::Mark { state, ids }) => cmd_mark(&path, &state, &ids),
        Some(Cmd::Tags) => cmd_tags(&path),
        Some(Cmd::Subscribe { url, folder }) => cmd_subscribe(&path, &url, folder).await,
        Some(Cmd::Refresh { feed }) => cmd_refresh(&path, feed).await,
    }
}

// ───────────────────────────── commands ─────────────────────────────

/// No-args home view: identify the tool, then show live unread state so an
/// agent can act immediately (AXI "content first").
fn cmd_home(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (unread, starred, later) = db::smart_counts(&conn).map_err(db_err)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;

    // Most recent unread first; fall back to recent reads when the inbox is clear.
    let recent = db::list_articles(&conn, &ArticleQuery::All, true, None, false, 10, 0)
        .map_err(db_err)?;
    let inbox_clear = recent.is_empty();
    let recent = if inbox_clear {
        db::list_articles(&conn, &ArticleQuery::All, false, None, false, 10, 0).map_err(db_err)?
    } else {
        recent
    };

    let mut out = Out::new();
    out.kv(0, "bin", &collapse_home(&current_exe_path()));
    out.kv(0, "description", DESCRIPTION);
    out.kv(0, "db", &collapse_home(&path.display().to_string()));
    out.line(
        0,
        &format!("unread: {unread} · starred: {starred} · later: {later}"),
    );
    out.kv(0, "feeds", &feeds.len().to_string());

    if inbox_clear {
        out.line(0, "inbox: 0 unread — all caught up");
        out.header(0, &format!("recent[{}]", recent.len()));
    }
    article_table(
        &mut out,
        if inbox_clear { "recent" } else { "unread" },
        &recent,
    );

    out.help(&[
        "Run `papr read <id>` to read an article's full text".into(),
        "Run `papr list --feed <id>` to list one feed's articles".into(),
        "Run `papr search \"<query>\"` to search every article".into(),
        "Run `papr refresh` to fetch new articles".into(),
    ]);
    Ok(out.into_string())
}

fn cmd_feeds(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let feeds = db::list_feeds(&conn).map_err(db_err)?;
    let folders = db::list_folders(&conn).map_err(db_err)?;

    if feeds.is_empty() {
        let mut out = Out::new();
        out.line(0, "feeds: 0 subscriptions yet");
        out.help(&["Run `papr subscribe <url>` to add your first feed".into()]);
        return Ok(out.into_string());
    }

    let total_unread: i64 = feeds.iter().map(|f| f.unread_count).sum();
    let mut out = Out::new();
    out.line(
        0,
        &format!("count: {} feeds · {total_unread} unread", feeds.len()),
    );

    // Group by folder so an agent sees the organisation; folderless feeds last.
    let folder_name = |id: Option<i64>| -> Option<String> {
        id.and_then(|fid| folders.iter().find(|f| f.id == fid).map(|f| f.name.clone()))
    };
    let mut printed_any_group = false;
    for folder in &folders {
        let group: Vec<_> = feeds.iter().filter(|f| f.folder_id == Some(folder.id)).collect();
        if group.is_empty() {
            continue;
        }
        printed_any_group = true;
        out.header(0, &scalar(&folder.name));
        feed_table(&mut out, 1, &group);
    }
    let loose: Vec<_> = feeds.iter().filter(|f| folder_name(f.folder_id).is_none()).collect();
    if !loose.is_empty() {
        if printed_any_group {
            out.header(0, "(no folder)");
            feed_table(&mut out, 1, &loose);
        } else {
            feed_table(&mut out, 0, &loose);
        }
    }

    out.help(&[
        "Run `papr list --feed <id>` to list a feed's articles".into(),
        "Run `papr refresh --feed <id>` to fetch one feed".into(),
        "Run `papr subscribe <url>` to add a feed".into(),
    ]);
    Ok(out.into_string())
}

fn cmd_list(path: &Path, args: ListArgs) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let (query, unread_only) = resolve_query(&args);
    let rows = db::list_articles(&conn, &query, unread_only, None, false, args.limit, args.offset)
        .map_err(db_err)?;
    let total = count_articles(&conn, &query, unread_only).map_err(db_err)?;

    if rows.is_empty() {
        let mut out = Out::new();
        out.line(0, &format!("articles: 0 {} found", scope_label(&query, unread_only)));
        out.help(&["Run `papr refresh` to fetch new articles".into()]);
        return Ok(out.into_string());
    }

    let mut out = Out::new();
    let shown = rows.len();
    out.line(
        0,
        &format!(
            "count: {shown} of {total} {}",
            scope_label(&query, unread_only)
        ),
    );
    article_table(&mut out, "articles", &rows);

    let mut help = vec![
        "Run `papr read <id>` to read an article's full text".into(),
        "Run `papr mark read <id>` to mark an article read".into(),
    ];
    if args.offset + (shown as i64) < total {
        help.push(format!(
            "Run `papr list --offset {} {}` for the next page",
            args.offset + args.limit,
            replay_filters(&args)
        ));
    }
    out.help(&help);
    Ok(out.into_string())
}

fn cmd_read(path: &Path, args: ReadArgs) -> Result<String, AxiError> {
    let conn = open_ro(path)?;

    // Resolve the id set: explicit ids win; otherwise pull the latest by filter.
    let ids: Vec<i64> = if !args.ids.is_empty() {
        args.ids.clone()
    } else if args.feed.is_some() || args.folder.is_some() || args.tag.is_some() {
        let query = if let Some(f) = args.feed {
            ArticleQuery::Feed(f)
        } else if let Some(f) = args.folder {
            ArticleQuery::Folder(f)
        } else {
            ArticleQuery::Tag(args.tag.unwrap())
        };
        db::list_articles(&conn, &query, args.unread, None, false, args.limit, 0)
            .map_err(db_err)?
            .into_iter()
            .map(|a| a.id)
            .collect()
    } else {
        return Err(AxiError::usage(
            "read needs an article id or a filter",
            vec![
                "Run `papr read <id> [<id>...]` to read by id".into(),
                "Run `papr read --feed <id> --limit 5` to read a feed's latest".into(),
            ],
        ));
    };

    if ids.is_empty() {
        let mut out = Out::new();
        out.line(0, "articles: 0 matched that filter");
        out.help(&["Run `papr list` to find article ids".into()]);
        return Ok(out.into_string());
    }

    let batch = ids.len() > 1;
    let budget = if batch { BATCH_TRUNCATE } else { READ_TRUNCATE };
    let mut out = Out::new();
    if batch {
        out.line(0, &format!("count: {} articles", ids.len()));
    }
    let mut any_truncated = false;
    for id in &ids {
        let detail = match db::get_article(&conn, *id) {
            Ok(d) => d,
            Err(_) => {
                out.header(0, "article");
                out.kv(1, "id", &id.to_string());
                out.kv(1, "error", "not found");
                continue;
            }
        };
        let (_title, text) = db::article_text(&conn, *id).map_err(db_err)?;
        out.header(0, "article");
        out.kv(1, "id", &detail.id.to_string());
        out.kv(1, "feed", &detail.feed_title);
        out.kv(1, "title", &detail.title);
        out.kv(1, "author", &opt(detail.author.as_deref()));
        out.kv(1, "url", &opt(detail.url.as_deref()));
        out.kv(1, "published", &opt(detail.published_at.as_deref()));
        out.kv(1, "state", &detail_flags(&detail));
        if !detail.tags.is_empty() {
            let names: Vec<String> = detail.tags.iter().map(|t| t.name.clone()).collect();
            out.kv(1, "tags", &names.join(", "));
        }
        let (shown, total_chars, truncated) = truncate(&text, if args.full { usize::MAX } else { budget });
        any_truncated |= truncated;
        let body = if truncated {
            format!("{shown}\n... (truncated, {total_chars} chars total)")
        } else {
            shown
        };
        out.block(1, "text", &body);
    }

    if any_truncated && !args.full {
        out.help(&["Run `papr read <id> --full` to see the complete text".into()]);
    }
    Ok(out.into_string())
}

fn cmd_search(path: &Path, query: &str, limit: i64) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let hits = db::search_articles_for_rag(&conn, query, limit).map_err(db_err)?;
    if hits.is_empty() {
        let mut out = Out::new();
        out.line(0, &format!("search: 0 matches for {}", scalar(query)));
        out.help(&["Run `papr refresh` to fetch new articles, then search again".into()]);
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} matches", hits.len()));
    let rows: Vec<Vec<String>> = hits
        .iter()
        .map(|(id, title, feed)| vec![id.to_string(), scalar(feed), scalar(&cap(title, 80))])
        .collect();
    out.table(0, "matches", &["id", "feed", "title"], &rows);
    out.help(&["Run `papr read <id>` to read a match in full".into()]);
    Ok(out.into_string())
}

fn cmd_mark(path: &Path, state: &str, ids: &[i64]) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let mut out = Out::new();
    let mut lines = Vec::new();
    for &id in ids {
        // Read current state for idempotency: an already-applied change is a
        // no-op with exit 0, not an error.
        let current = match db::get_article(&conn, id) {
            Ok(d) => d,
            Err(_) => {
                lines.push(format!("#{id}: not found"));
                continue;
            }
        };
        let (already, result) = match state {
            "read" => (current.is_read, db::set_read(&conn, id, true)),
            "unread" => (!current.is_read, db::set_read(&conn, id, false)),
            "star" => (current.is_starred, db::set_starred(&conn, id, true)),
            "unstar" => (!current.is_starred, db::set_starred(&conn, id, false)),
            "later" => (current.read_later, db::set_read_later(&conn, id, true)),
            "unlater" => (!current.read_later, db::set_read_later(&conn, id, false)),
            _ => unreachable!("clap restricts the state value"),
        };
        result.map_err(db_err)?;
        if already {
            lines.push(format!("#{id}: already {state} (no-op)"));
        } else {
            lines.push(format!("#{id}: {state}"));
        }
    }
    out.header(0, &format!("marked[{}]", lines.len()));
    for l in &lines {
        out.line(1, l);
    }
    Ok(out.into_string())
}

fn cmd_tags(path: &Path) -> Result<String, AxiError> {
    let conn = open_ro(path)?;
    let tags = db::list_tags(&conn).map_err(db_err)?;
    if tags.is_empty() {
        let mut out = Out::new();
        out.line(0, "tags: 0 defined");
        return Ok(out.into_string());
    }
    let mut out = Out::new();
    out.line(0, &format!("count: {} tags", tags.len()));
    let rows: Vec<Vec<String>> = tags
        .iter()
        .map(|t| vec![t.id.to_string(), scalar(&t.name), t.article_count.to_string()])
        .collect();
    out.table(0, "tags", &["id", "name", "articles"], &rows);
    out.help(&["Run `papr list --tag <id>` to list a tag's articles".into()]);
    Ok(out.into_string())
}

async fn cmd_subscribe(path: &Path, url: &str, folder: Option<i64>) -> Result<String, AxiError> {
    let conn = open_rw(path)?;
    let client = http_client()?;

    // Fetch the target. If it parses as a feed, use it directly; otherwise treat
    // it as an HTML page and auto-discover the feed link.
    let (bytes, _etag, final_url) = fetch::get(&client, url)
        .await
        .map_err(|e| AxiError::runtime(format!("could not fetch {url}: {e}")))?;

    let (feed_url, parsed) = match parse::parse_feed(&bytes, &final_url) {
        Ok(parsed) => (final_url.clone(), parsed),
        Err(_) => {
            // Not a feed document — look for <link rel=alternate> feeds.
            let html = String::from_utf8_lossy(&bytes);
            let candidates = parse::discover_feeds(&html, &final_url);
            let Some(candidate) = candidates.into_iter().next() else {
                return Err(AxiError::runtime_help(
                    format!("no feed found at {url}"),
                    vec!["Pass the feed URL directly if you have it".into()],
                ));
            };
            let (fbytes, _e, furl) = fetch::get(&client, &candidate)
                .await
                .map_err(|e| AxiError::runtime(format!("could not fetch {candidate}: {e}")))?;
            let parsed = parse::parse_feed(&fbytes, &furl)
                .map_err(|e| AxiError::runtime(format!("discovered feed did not parse: {e}")))?;
            (furl, parsed)
        }
    };

    // Idempotent: re-subscribing to a known feed is a no-op, not an error.
    if let Some(existing) = db::find_feed_by_url(&conn, &feed_url).map_err(db_err)? {
        let mut out = Out::new();
        out.line(0, &format!("feed: #{existing} already subscribed (no-op)"));
        out.help(&[format!("Run `papr list --feed {existing}` to read it")]);
        return Ok(out.into_string());
    }

    let source_type =
        parse::refine_source_type(papr_core::models::SourceType::Rss, &parsed, &feed_url);
    let title = parsed
        .title
        .clone()
        .unwrap_or_else(|| feed_url.clone());
    let feed_id = db::insert_feed(
        &conn,
        &feed_url,
        parsed.site_url.as_deref(),
        &title,
        parsed.description.as_deref(),
        source_type,
        folder,
    )
    .map_err(db_err)?;

    // Initial population: ingest the articles we already parsed.
    let rules = db::active_rules(&conn).unwrap_or_default();
    let dedup = db::setting_flag(&conn, "dedup_enabled", false);
    let mut new_count = 0usize;
    for article in &parsed.articles {
        if let Ok(true) = db::upsert_article(&conn, feed_id, article, dedup, &rules) {
            new_count += 1;
        }
    }
    let _ = db::touch_feed(&conn, feed_id);

    let mut out = Out::new();
    out.header(0, "feed");
    out.kv(1, "id", &feed_id.to_string());
    out.kv(1, "title", &title);
    out.kv(1, "url", &feed_url);
    out.kv(1, "type", source_type.as_str());
    out.kv(1, "articles", &new_count.to_string());
    out.help(&[
        format!("Run `papr list --feed {feed_id}` to read it"),
        format!("Run `papr refresh --feed {feed_id}` to fetch more later"),
    ]);
    Ok(out.into_string())
}

async fn cmd_refresh(path: &Path, feed: Option<i64>) -> Result<String, AxiError> {
    let conn = db::open(path).map_err(db_err)?;
    let dbm = tokio::sync::Mutex::new(conn);
    let client = http_client()?;
    let scope = match feed {
        Some(id) => refresh::RefreshScope::One(id),
        None => refresh::RefreshScope::All,
    };

    // Progress is diagnostic — it goes to stderr so stdout stays pure data.
    let summary = refresh::refresh_core(&dbm, &client, scope, |event| {
        use papr_core::models::RefreshProgress::*;
        match event {
            Started { total } => eprintln!("refreshing {total} feed(s)…"),
            FeedDone { feed_id, new_articles, error } => {
                if let Some(e) = error {
                    eprintln!("  feed {feed_id}: error — {e}");
                } else if new_articles > 0 {
                    eprintln!("  feed {feed_id}: {new_articles} new");
                }
            }
            Finished { new_articles } => eprintln!("done — {new_articles} new article(s)"),
        }
    })
    .await
    .map_err(db_err)?;

    let mut out = Out::new();
    out.header(0, "refresh");
    out.kv(1, "scope", &feed.map(|f| format!("feed {f}")).unwrap_or_else(|| "all".into()));
    out.kv(1, "new", &summary.new_articles.to_string());
    if summary.new_articles > 0 {
        out.help(&["Run `papr list` to see what's new".into()]);
    }
    Ok(out.into_string())
}

// ───────────────────────────── rendering helpers ─────────────────────────────

/// `{id,feed,title,flags,date}` — the minimal schema an agent needs to pick a
/// next action without a follow-up call.
fn article_table(out: &mut Out, name: &str, rows: &[papr_core::models::ArticleSummary]) {
    let table_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|a| {
            vec![
                a.id.to_string(),
                scalar(&cap(&a.feed_title, 28)),
                scalar(&cap(&a.title, 80)),
                summary_flags(a),
                short_date(a.published_at.as_deref()),
            ]
        })
        .collect();
    out.table(0, name, &["id", "feed", "title", "flags", "date"], &table_rows);
}

fn feed_table(out: &mut Out, depth: usize, feeds: &[&papr_core::models::Feed]) {
    let rows: Vec<Vec<String>> = feeds
        .iter()
        .map(|f| {
            vec![
                f.id.to_string(),
                scalar(&cap(&f.title, 40)),
                f.unread_count.to_string(),
                f.source_type.clone(),
            ]
        })
        .collect();
    out.table(depth, "feeds", &["id", "title", "unread", "type"], &rows);
}

fn summary_flags(a: &papr_core::models::ArticleSummary) -> String {
    let mut v = vec![if a.is_read { "read" } else { "unread" }];
    if a.is_starred {
        v.push("star");
    }
    if a.read_later {
        v.push("later");
    }
    v.join(".")
}

fn detail_flags(a: &papr_core::models::ArticleDetail) -> String {
    let mut v = vec![if a.is_read { "read" } else { "unread" }];
    if a.is_starred {
        v.push("star");
    }
    if a.read_later {
        v.push("later");
    }
    v.join(".")
}

/// Map list/read filter flags to a `(query, unread_only)` pair.
fn resolve_query(args: &ListArgs) -> (ArticleQuery, bool) {
    let query = if let Some(f) = args.feed {
        ArticleQuery::Feed(f)
    } else if let Some(f) = args.folder {
        ArticleQuery::Folder(f)
    } else if let Some(t) = args.tag {
        ArticleQuery::Tag(t)
    } else if args.starred {
        ArticleQuery::Starred
    } else if args.later {
        ArticleQuery::ReadLater
    } else {
        ArticleQuery::All
    };
    // Starred / read-later views show regardless of read state; everything else
    // defaults to unread unless --all is given.
    let unread_only = !args.all && !args.starred && !args.later;
    (query, unread_only)
}

fn scope_label(query: &ArticleQuery, unread_only: bool) -> String {
    let base = match query {
        ArticleQuery::Starred => "starred",
        ArticleQuery::ReadLater => "read-later",
        ArticleQuery::Feed(_) => "in feed",
        ArticleQuery::Folder(_) => "in folder",
        ArticleQuery::Tag(_) => "tagged",
        _ => "articles",
    };
    if unread_only {
        format!("unread {base}").replace("unread articles", "unread")
    } else {
        base.to_string()
    }
}

/// A count(*) mirroring a list query + the unread filter, so the list header can
/// state a definitive total instead of forcing the agent to paginate to find out.
fn count_articles(
    conn: &Connection,
    query: &ArticleQuery,
    unread_only: bool,
) -> papr_core::error::AppResult<i64> {
    let unread = if unread_only { " AND is_read = 0" } else { "" };
    let (sql, param): (String, Option<i64>) = match query {
        ArticleQuery::All | ArticleQuery::Unread => {
            (format!("SELECT count(*) FROM articles WHERE 1=1{unread}"), None)
        }
        ArticleQuery::Starred => (
            format!("SELECT count(*) FROM articles WHERE is_starred = 1{unread}"),
            None,
        ),
        ArticleQuery::ReadLater => (
            format!("SELECT count(*) FROM articles WHERE read_later = 1{unread}"),
            None,
        ),
        ArticleQuery::Feed(id) => (
            format!("SELECT count(*) FROM articles WHERE feed_id = ?1{unread}"),
            Some(*id),
        ),
        ArticleQuery::Folder(id) => (
            format!(
                "SELECT count(*) FROM articles WHERE feed_id IN \
                 (SELECT id FROM feeds WHERE folder_id = ?1){unread}"
            ),
            Some(*id),
        ),
        ArticleQuery::Tag(id) => (
            format!(
                "SELECT count(*) FROM articles WHERE id IN \
                 (SELECT article_id FROM article_tags WHERE tag_id = ?1){unread}"
            ),
            Some(*id),
        ),
    };
    let count = match param {
        Some(p) => conn.query_row(&sql, [p], |r| r.get::<_, i64>(0))?,
        None => conn.query_row(&sql, [], |r| r.get::<_, i64>(0))?,
    };
    Ok(count)
}

/// Reproduce the active filter flags so a pagination hint carries them forward.
fn replay_filters(args: &ListArgs) -> String {
    let mut parts = Vec::new();
    if let Some(f) = args.feed {
        parts.push(format!("--feed {f}"));
    }
    if let Some(f) = args.folder {
        parts.push(format!("--folder {f}"));
    }
    if let Some(t) = args.tag {
        parts.push(format!("--tag {t}"));
    }
    if args.starred {
        parts.push("--starred".into());
    }
    if args.later {
        parts.push("--later".into());
    }
    if args.all {
        parts.push("--all".into());
    }
    parts.join(" ")
}

/// Truncate on a char boundary, returning `(shown, total_chars, was_truncated)`.
fn truncate(text: &str, budget: usize) -> (String, usize, bool) {
    let total = text.chars().count();
    if total <= budget {
        return (text.to_string(), total, false);
    }
    let shown: String = text.chars().take(budget).collect();
    (shown, total, true)
}

/// Cap a single-line cell, appending an ellipsis when shortened.
fn cap(s: &str, n: usize) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.chars().count() <= n {
        return one_line;
    }
    let mut t: String = one_line.chars().take(n.saturating_sub(1)).collect();
    t.push('…');
    t
}

fn short_date(published: Option<&str>) -> String {
    match published {
        Some(s) if s.len() >= 10 => s[..10].to_string(),
        Some(s) => scalar(s),
        None => "-".to_string(),
    }
}

// ───────────────────────────── infrastructure ─────────────────────────────

fn http_client() -> Result<reqwest::Client, AxiError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| AxiError::runtime(format!("http client: {e}")))
}

fn open_ro(path: &Path) -> Result<Connection, AxiError> {
    ensure_db(path)?;
    db::open_reader(path).map_err(db_err)
}

fn open_rw(path: &Path) -> Result<Connection, AxiError> {
    ensure_db(path)?;
    db::open(path).map_err(db_err)
}

fn ensure_db(path: &Path) -> Result<(), AxiError> {
    if path.exists() {
        return Ok(());
    }
    Err(AxiError::runtime_help(
        format!("no Papr database at {}", collapse_home(&path.display().to_string())),
        vec![
            "Install and launch Papr to create it, or".into(),
            "Run any papr command with `--db <path>` / set PAPR_DB to point at one".into(),
        ],
    ))
}

fn db_path(cli: &Cli) -> Result<PathBuf, AxiError> {
    if let Some(p) = &cli.db {
        return Ok(p.clone());
    }
    Ok(app_data_dir()?.join(APP_IDENTIFIER).join("papr.db"))
}

/// The platform application-data directory Tauri stores the DB under.
fn app_data_dir() -> Result<PathBuf, AxiError> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| home_err())?;
        Ok(PathBuf::from(home).join("Library/Application Support"))
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").map_err(|_| home_err())?;
        Ok(PathBuf::from(appdata))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Ok(x) = std::env::var("XDG_DATA_HOME") {
            if !x.is_empty() {
                return Ok(PathBuf::from(x));
            }
        }
        let home = std::env::var("HOME").map_err(|_| home_err())?;
        Ok(PathBuf::from(home).join(".local/share"))
    }
}

fn home_err() -> AxiError {
    AxiError::runtime_help(
        "could not resolve the home directory",
        vec!["Set PAPR_DB or pass --db <path> to the database".into()],
    )
}

fn current_exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "papr".into())
}

/// Collapse the user's home prefix to `~` for compact, portable display.
fn collapse_home(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            if let Some(rest) = p.strip_prefix(&home) {
                return format!("~{rest}");
            }
        }
    }
    p.to_string()
}

// ───────────────────────────── errors ─────────────────────────────

/// A structured, agent-readable error. Rendered to **stdout** in TOON (same
/// channel as success) so the agent can read and act on it; exit code conveys
/// the class (1 = runtime, 2 = usage).
struct AxiError {
    message: String,
    help: Vec<String>,
    usage: bool,
}

impl AxiError {
    fn runtime(message: impl Into<String>) -> Self {
        Self { message: message.into(), help: Vec::new(), usage: false }
    }
    fn runtime_help(message: impl Into<String>, help: Vec<String>) -> Self {
        Self { message: message.into(), help, usage: false }
    }
    fn usage(message: impl Into<String>, help: Vec<String>) -> Self {
        Self { message: message.into(), help, usage: true }
    }
    fn exit_code(&self) -> ExitCode {
        if self.usage {
            ExitCode::from(2)
        } else {
            ExitCode::FAILURE
        }
    }
}

/// Translate a database/core error into a runtime AxiError, discarding noisy
/// internals — the agent gets actionable meaning, not a stack trace.
fn db_err(e: papr_core::error::AppError) -> AxiError {
    AxiError::runtime(format!("{e}"))
}

fn render_error(e: &AxiError) -> String {
    let mut out = Out::new();
    out.kv(0, "error", &e.message);
    out.help(&e.help);
    out.into_string()
}
