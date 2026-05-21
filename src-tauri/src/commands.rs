//! Tauri command surface — the typed IPC boundary the React frontend calls.
//! All SQL is delegated to `db`; commands only orchestrate.

use crate::ai::{self, AiConfig, AiEvent};
use crate::db::{self};
use crate::error::{AppError, AppResult};
use crate::export::{self, ExportArticle};
use crate::extraction;
use crate::share::{self, KindleConfig, ShareArticle};
use crate::ingestion::discovery::{self, DiscoveryResult};
use crate::ingestion::newsletter::{self, NewsletterConfig};
use crate::ingestion::sources::{self, Normalized};
use crate::ingestion::{fetch, parse, scheduler};
use crate::models::*;
use crate::opml;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{ipc::Channel, AppHandle, Emitter, Manager, State};
use url::Url;

// ─────────────────────────── folders ───────────────────────────

#[tauri::command]
pub async fn list_folders(state: State<'_, AppState>) -> AppResult<Vec<Folder>> {
    let conn = state.read().await;
    db::list_folders(&conn)
}

#[tauri::command]
pub async fn create_folder(state: State<'_, AppState>, name: String) -> AppResult<i64> {
    let conn = state.db.lock().await;
    db::create_folder(&conn, &name)
}

#[tauri::command]
pub async fn rename_folder(state: State<'_, AppState>, id: i64, name: String) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::rename_folder(&conn, id, &name)
}

#[tauri::command]
pub async fn delete_folder(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_folder(&conn, id)
}

// ─────────────────────────── feeds ───────────────────────────

#[tauri::command]
pub async fn list_feeds(state: State<'_, AppState>) -> AppResult<Vec<Feed>> {
    let conn = state.read().await;
    db::list_feeds(&conn)
}

/// Subscribe to a feed. Accepts either a feed URL or a website URL (in which
/// case we auto-discover the feed). Also recognizes multi-source URLs —
/// YouTube channels, subreddits, Mastodon profiles — and rewrites them to the
/// real feed URL first (feature F5). Fetches once so the feed is immediately
/// populated, then returns the stored feed.
#[tauri::command]
pub async fn add_feed(
    state: State<'_, AppState>,
    url: String,
    folder_id: Option<i64>,
) -> AppResult<Feed> {
    let client = state.http();

    // Step 0: multi-source normalization. If the pasted URL is a known
    // special source, rewrite it to its real feed URL. A YouTube vanity URL
    // needs the channel page fetched to learn its channel id; that single
    // network call lives here (the extraction logic itself is pure).
    let (effective_url, forced_type): (String, Option<SourceType>) =
        match sources::normalize_source(&url) {
            Normalized::Feed { url, source_type } => (url, Some(source_type)),
            Normalized::NeedsYoutubeResolution { page_url } => {
                let (page_bytes, ct, _) = fetch::get(&client, &page_url).await?;
                let html = fetch::decode_html(&page_bytes, ct.as_deref());
                let channel_id = sources::extract_channel_id(&html)
                    .ok_or_else(|| AppError::code("youtubeChannelNotFound"))?;
                (
                    sources::youtube_feed_url(&channel_id),
                    Some(SourceType::Youtube),
                )
            }
            Normalized::Untouched => (url.clone(), None),
        };

    // Step 1: fetch whatever the user gave us (or the normalized feed URL).
    let (bytes, ct, final_url) = fetch::get(&client, &effective_url).await?;

    // Step 2: if it is a feed use it directly, otherwise discover one.
    let (feed_url, feed_bytes) = if parse::looks_like_feed(&bytes) {
        (final_url, bytes)
    } else {
        let html = fetch::decode_html(&bytes, ct.as_deref());
        let candidates = parse::discover_feeds(&html, &final_url);
        let candidate = candidates
            .into_iter()
            .next()
            .ok_or_else(|| AppError::code("noFeedFound"))?;
        let (fb, _, _) = fetch::get(&client, &candidate).await?;
        (candidate, fb)
    };

    // Step 3: parse and classify. A normalization step that already pinned a
    // source type (YouTube / Reddit / Mastodon) wins over heuristic detection.
    let parsed = parse::parse_feed(&feed_bytes, &feed_url)?;
    let source_type = match forced_type {
        Some(t) => t,
        None => parse::refine_source_type(
            parse::detect_source_type(&feed_url),
            &parsed,
            &feed_url,
        ),
    };

    let title = parsed
        .title
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| feed_url.clone());
    let favicon = parsed.icon.clone().or_else(|| {
        parsed
            .site_url
            .as_deref()
            .and_then(|s| Url::parse(s).ok())
            .and_then(|u| u.host_str().map(String::from))
            .map(|h| format!("https://www.google.com/s2/favicons?domain={h}&sz=64"))
    });

    // Step 4: persist.
    let conn = state.db.lock().await;
    if db::find_feed_by_url(&conn, &feed_url)?.is_some() {
        return Err(AppError::code("alreadySubscribed"));
    }
    let feed_id = db::insert_feed(
        &conn,
        &feed_url,
        parsed.site_url.as_deref(),
        &title,
        parsed.description.as_deref(),
        source_type,
        folder_id,
    )?;
    if let Some(fav) = &favicon {
        db::update_feed_meta(&conn, feed_id, None, None, None, Some(fav))?;
    }
    let dedup = db::setting_flag(&conn, "dedup_enabled", false);
    let rules = db::active_rules(&conn).unwrap_or_default();
    for article in &parsed.articles {
        db::upsert_article(&conn, feed_id, article, dedup, &rules)?;
    }
    // Record that the feed was just fetched. `add_feed` fetches the document
    // here in step 1/2, so without this `last_fetched_at` would stay NULL —
    // the feed would wrongly read as "never refreshed" until the next
    // scheduler tick, and the tick would also re-fetch it in full a moment
    // after this add. (The conditional-GET revalidators are not captured —
    // `fetch::get` does not surface ETag / Last-Modified — so the next poll
    // does one full GET before it can store them; that is a single missed
    // optimisation, not incorrect behaviour, and many feeds send no ETag at
    // all.)
    let _ = db::touch_feed(&conn, feed_id);
    let last_fetched_at = db::feed_last_fetched(&conn, feed_id).ok().flatten();
    // Count actual unread rows rather than tallying insertions: keeps the
    // returned `unread_count` aligned with the sidebar's `list_feeds` count
    // regardless of how filter rules pre-set article state.
    let unread = db::count_feed_unread(&conn, feed_id)?;
    drop(conn);

    Ok(Feed {
        id: feed_id,
        feed_url,
        site_url: parsed.site_url,
        title,
        description: parsed.description,
        favicon_url: favicon,
        folder_id,
        source_type: source_type.as_str().to_string(),
        last_fetched_at,
        fetch_error: None,
        unread_count: unread,
    })
}

/// Feed discovery (feature F6). Searches the bundled curated directory for
/// entries matching `query`, scoped to `lang` (the user's UI language) so the
/// recommendations are in a language they read. When `query` looks like a URL
/// or bare domain we ALSO fetch that page and run `parse::discover_feeds` over
/// it, so the same box doubles as smart URL handling. Live page-scrape results
/// are returned first (most specific to what the user typed), then the
/// directory matches.
///
/// A failed page fetch is non-fatal — the directory results are still
/// returned — so a typo or an offline site does not break discovery.
#[tauri::command]
pub async fn search_feed_directory(
    state: State<'_, AppState>,
    query: String,
    lang: String,
) -> AppResult<Vec<DiscoveryResult>> {
    let mut results: Vec<DiscoveryResult> = Vec::new();

    // Live page scrape — only when the query genuinely looks like a URL.
    if discovery::looks_like_url(&query) {
        let target = discovery::normalize_query_url(&query);
        let client = state.http();
        if let Ok((bytes, ct, final_url)) = fetch::get(&client, &target).await {
            if parse::looks_like_feed(&bytes) {
                // The pasted URL is itself a feed — surface it directly.
                let title = parse::parse_feed(&bytes, &final_url)
                    .ok()
                    .and_then(|p| p.title);
                results.push(DiscoveryResult::from_scrape(final_url, title));
            } else {
                let html = fetch::decode_html(&bytes, ct.as_deref());
                for feed_url in parse::discover_feeds(&html, &final_url) {
                    results.push(DiscoveryResult::from_scrape(feed_url, None));
                }
            }
        }
    }

    // Curated directory matches (deduplicated against the scrape results),
    // scoped to the user's UI language so the recommendations read natively.
    for hit in discovery::search_directory(&query, &lang) {
        if !results.iter().any(|r| r.feed_url == hit.feed_url) {
            results.push(hit);
        }
    }
    Ok(results)
}

#[tauri::command]
pub async fn delete_feed(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_feed(&conn, id)
}

#[tauri::command]
pub async fn move_feed(
    state: State<'_, AppState>,
    id: i64,
    folder_id: Option<i64>,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::move_feed(&conn, id, folder_id)
}

#[tauri::command]
pub async fn rename_feed(state: State<'_, AppState>, id: i64, title: String) -> AppResult<()> {
    // `db::rename_feed` trims and rejects an empty title — the one chokepoint.
    let conn = state.db.lock().await;
    db::rename_feed(&conn, id, &title)
}

/// Refresh every feed, streaming progress to the frontend over `on_progress`.
#[tauri::command]
pub async fn refresh_feeds(
    app: AppHandle,
    on_progress: Channel<RefreshProgress>,
) -> AppResult<usize> {
    scheduler::refresh_all(&app, Some(on_progress), false).await
}

// ─────────────────────────── articles ───────────────────────────

#[tauri::command]
pub async fn list_articles(
    state: State<'_, AppState>,
    query: ArticleQuery,
    unread_only: bool,
    search: Option<String>,
    oldest_first: bool,
    limit: i64,
    offset: i64,
) -> AppResult<Vec<ArticleSummary>> {
    let conn = state.read().await;
    db::list_articles(
        &conn,
        &query,
        unread_only,
        search.as_deref(),
        oldest_first,
        limit,
        offset,
    )
}

#[tauri::command]
pub async fn get_article(state: State<'_, AppState>, id: i64) -> AppResult<ArticleDetail> {
    let conn = state.read().await;
    db::get_article(&conn, id)
}

/// Queue a read/starred change for FreshRSS, but only when a server is linked.
fn enqueue_if_connected(conn: &rusqlite::Connection, id: i64, field: &str, value: bool) {
    if db::is_freshrss_connected(conn) {
        let _ = db::enqueue_sync(conn, id, field, value);
    }
}

/// Refresh the two unread surfaces — the Dock badge and the menu-bar tray —
/// after an operation that changed the unread count.
async fn refresh_unread_surfaces(app: &AppHandle) {
    crate::notify::update_badge(app).await;
    crate::tray::refresh(app).await;
}

#[tauri::command]
pub async fn mark_read(app: AppHandle, id: i64, read: bool) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::set_read(&conn, id, read)?;
        enqueue_if_connected(&conn, id, "read", read);
    }
    refresh_unread_surfaces(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn mark_starred(app: AppHandle, id: i64, starred: bool) -> AppResult<()> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().await;
    db::set_starred(&conn, id, starred)?;
    enqueue_if_connected(&conn, id, "starred", starred);
    Ok(())
}

#[tauri::command]
pub async fn mark_read_later(state: State<'_, AppState>, id: i64, value: bool) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::set_read_later(&conn, id, value)
}

#[tauri::command]
pub async fn mark_all_read(app: AppHandle, query: ArticleQuery) -> AppResult<usize> {
    let n = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::mark_all_read(&conn, &query, db::is_freshrss_connected(&conn))?
    };
    let _ = app.emit("feeds-updated", 0);
    refresh_unread_surfaces(&app).await;
    Ok(n)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartCounts {
    unread: i64,
    starred: i64,
    read_later: i64,
}

#[tauri::command]
pub async fn smart_counts(state: State<'_, AppState>) -> AppResult<SmartCounts> {
    let conn = state.read().await;
    let (unread, starred, read_later) = db::smart_counts(&conn)?;
    Ok(SmartCounts {
        unread,
        starred,
        read_later,
    })
}

// ─────────────────────────── full-text extraction ───────────────────────────

/// Fetch the article's source page and extract its full text (Readability).
/// Stores the result so subsequent reads are instant/offline.
#[tauri::command]
pub async fn extract_fulltext(state: State<'_, AppState>, article_id: i64) -> AppResult<String> {
    let url = {
        let conn = state.read().await;
        db::get_article(&conn, article_id)?
            .url
            .ok_or_else(|| AppError::code("noArticleUrl"))?
    };

    let http = state.http();
    let (bytes, ct, final_url) = fetch::get(&http, &url).await?;
    // Decode in the page's declared charset — a non-UTF-8 page (Shift-JIS,
    // GBK, ISO-8859-1, …) would otherwise become mojibake before Readability.
    let html = fetch::decode_html(&bytes, ct.as_deref());

    // Readability is not Send — run it on the blocking pool.
    let extracted =
        tokio::task::spawn_blocking(move || extraction::extract_article(&html, &final_url))
            .await
            .map_err(|e| AppError::other(format!("extraction task: {e}")))??;

    let conn = state.db.lock().await;
    db::set_extracted_html(&conn, article_id, &extracted)?;
    Ok(extracted)
}

// ─────────────────────────── OPML ───────────────────────────

#[tauri::command]
pub async fn import_opml(app: AppHandle, content: String) -> AppResult<usize> {
    let imported = opml::parse(&content)?;
    let count = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        // One transaction for the whole import — a mid-list failure rolls
        // back rather than leaving feeds (and auto-created folders) partly
        // imported.
        let tx = conn.unchecked_transaction()?;
        let mut added = 0;
        for feed in imported {
            if db::find_feed_by_url(&tx, &feed.feed_url)?.is_some() {
                continue;
            }
            let folder_id = match &feed.folder {
                Some(name) => Some(db::folder_id_by_name(&tx, name)?),
                None => None,
            };
            let source_type = parse::detect_source_type(&feed.feed_url);
            db::insert_feed(
                &tx,
                &feed.feed_url,
                None,
                &feed.title,
                None,
                source_type,
                folder_id,
            )?;
            added += 1;
        }
        tx.commit()?;
        added
    };
    // Newly imported feeds have no articles yet — kick off a refresh. Pass
    // wait_if_busy so it queues behind any in-flight refresh instead of
    // skipping and leaving the imported feeds empty until the next tick.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = scheduler::refresh_all(&app2, None, true).await;
    });
    Ok(count)
}

#[tauri::command]
pub async fn export_opml(state: State<'_, AppState>) -> AppResult<String> {
    let conn = state.read().await;
    let feeds = db::feeds_for_export(&conn)?;
    opml::build(&feeds)
}

// ─────────────────────────── settings ───────────────────────────

#[tauri::command]
pub async fn get_setting(state: State<'_, AppState>, key: String) -> AppResult<Option<String>> {
    let conn = state.read().await;
    db::get_setting(&conn, &key)
}

#[tauri::command]
pub async fn set_setting(
    state: State<'_, AppState>,
    key: String,
    value: String,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::set_setting(&conn, &key, &value)
}

// ─────────────────────────── AI ───────────────────────────

/// Load the AI provider configuration from the settings table.
fn load_ai_config(conn: &rusqlite::Connection) -> AppResult<AiConfig> {
    AiConfig::new(
        db::get_setting(conn, "ai_provider")?,
        db::get_setting(conn, "ai_api_key")?,
        db::get_setting(conn, "ai_model")?,
        db::get_setting(conn, "ai_base_url")?,
    )
}

/// Truncate to at most `max` characters without splitting a UTF-8 boundary.
fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// A system-prompt directive so AI output matches the UI language rather than
/// defaulting to whatever language the source article happens to be in.
fn response_language(conn: &rusqlite::Connection) -> &'static str {
    match db::get_setting(conn, "language").ok().flatten().as_deref() {
        Some("zh") => "\n\nAlways write your response in Simplified Chinese.",
        Some("ja") => "\n\nAlways write your response in Japanese.",
        _ => "\n\nAlways write your response in English.",
    }
}

/// Stream an AI summary of one article; the full summary is also persisted.
#[tauri::command]
pub async fn ai_summarize(
    state: State<'_, AppState>,
    article_id: i64,
    on_token: Channel<AiEvent>,
) -> AppResult<()> {
    let (title, body, cfg, lang) = {
        let conn = state.read().await;
        let (title, body) = db::article_text(&conn, article_id)?;
        (title, body, load_ai_config(&conn)?, response_language(&conn))
    };
    // A title-only item (link-aggregator posts, some podcast/video feeds carry
    // no body text) gives the model nothing to summarize. Without this guard it
    // would invent a "summary" from the bare title alone — and that fabricated
    // text would then be persisted to `ai_summary`. Bail out the same way
    // `ai_ask` / `ai_digest` do when their input is empty.
    if body.trim().is_empty() {
        return Err(AppError::code("noArticleBody"));
    }
    // The drawer renders the response as markdown (.ai-prose styles paragraphs,
    // bullets, and bold), so we ask for structured output instead of a single
    // dense paragraph — the reader can scan a TL;DR + bullets far faster.
    let system = format!(
        "You are a sharp news editor. Summarize the article so a reader can \
         decide whether to read it in full.\n\n\
         Format the response in markdown using exactly this shape:\n\
         **TL;DR** — One sentence capturing the single most important point.\n\n\
         - Key fact, finding, or claim (under ~20 words)\n\
         - Another key point\n\
         - 3 to 5 bullets total, one idea each, no nested bullets\n\n\
         Output only this structure. No preamble, no closing remarks, no \
         section headers, no extra prose.{lang}"
    );
    let user = format!("Title: {title}\n\n{}", truncate(&body, 8000));

    let http = state.http();
    let outcome = ai::stream_chat(&http, &cfg, &system, &user, &on_token).await?;
    // Persist only a summary that streamed to completion. If the user closed
    // the AI panel mid-stream the channel was dropped and `outcome.text` holds
    // just a truncated fragment — caching that would make the next open show a
    // broken half-summary with no way to regenerate it.
    if outcome.completed && !outcome.text.trim().is_empty() {
        let conn = state.db.lock().await;
        db::set_ai_summary(&conn, article_id, outcome.text.trim())?;
    }
    Ok(())
}

/// Answer a question using the user's subscribed articles as RAG context.
/// Retrieval currently uses FTS5 keyword search (semantic search is Phase 5).
#[tauri::command]
pub async fn ai_ask(
    state: State<'_, AppState>,
    question: String,
    on_token: Channel<AiEvent>,
) -> AppResult<()> {
    let (cfg, context, lang) = {
        let conn = state.read().await;
        let cfg = load_ai_config(&conn)?;
        // RAG retrieval is recall-oriented: match articles that share *any* of
        // the question's keywords. `list_articles` AND-joins every search word,
        // which for a natural-language question matches nothing.
        let hits = db::search_articles_for_rag(&conn, &question, 6)?;
        let mut context = String::new();
        for (id, _title, feed_title) in hits {
            let (title, body) = db::article_text(&conn, id)?;
            context.push_str(&format!(
                "## {} — {}\n{}\n\n",
                title,
                feed_title,
                truncate(&body, 1200)
            ));
        }
        (cfg, context, response_language(&conn))
    };

    let system = format!(
        "You answer the user's question using only the provided \
         articles from their RSS subscriptions. Cite the article \
         titles you draw from. If the articles do not contain the \
         answer, say so plainly.{lang}"
    );
    let user = if context.trim().is_empty() {
        format!("No relevant articles were found.\n\nQuestion: {question}")
    } else {
        format!("Articles from the user's feeds:\n\n{context}---\n\nQuestion: {question}")
    };

    let http = state.http();
    ai::stream_chat(&http, &cfg, &system, &user, &on_token).await?;
    Ok(())
}

/// Stream an AI briefing that synthesizes the most recent articles by theme.
#[tauri::command]
pub async fn ai_digest(
    state: State<'_, AppState>,
    on_token: Channel<AiEvent>,
) -> AppResult<()> {
    let (cfg, articles, lang) = {
        let conn = state.read().await;
        (
            load_ai_config(&conn)?,
            db::digest_source(&conn, 30)?,
            response_language(&conn),
        )
    };
    if articles.is_empty() {
        return Err(AppError::code("noArticles"));
    }

    let mut corpus = String::new();
    for (title, feed, text) in &articles {
        corpus.push_str(&format!("- [{feed}] {title}: {}\n", truncate(text, 400)));
    }

    let system = format!(
        "You are the user's personal news briefer. From the recent \
         articles, write a crisp briefing: group related items into \
         2-4 themed sections with short headers, lead with what \
         matters most, and keep it skimmable. Plain prose, no preamble.{lang}"
    );
    let user = format!("Recent articles from my feeds:\n\n{corpus}");

    let http = state.http();
    ai::stream_chat(&http, &cfg, &system, &user, &on_token).await?;
    Ok(())
}

// ─────────────────────────── storage ───────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStats {
    db_bytes: i64,
    article_count: i64,
    feed_count: i64,
}

#[tauri::command]
pub async fn storage_stats(state: State<'_, AppState>) -> AppResult<StorageStats> {
    let conn = state.read().await;
    let (db_bytes, article_count, feed_count) = db::storage_stats(&conn)?;
    Ok(StorageStats {
        db_bytes,
        article_count,
        feed_count,
    })
}

/// Delete read articles older than `days` (starred / read-later are kept).
#[tauri::command]
pub async fn cleanup_articles(app: AppHandle, days: i64) -> AppResult<usize> {
    let n = {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::cleanup_old_articles(&conn, days)?
    };
    let _ = app.emit("feeds-updated", 0);
    Ok(n)
}

/// Reclaim free database pages.
#[tauri::command]
pub async fn vacuum_db(state: State<'_, AppState>) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::vacuum(&conn)
}

/// Clear every stored setting (AI keys, sync credentials, preferences).
#[tauri::command]
pub async fn reset_settings(state: State<'_, AppState>) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::reset_settings(&conn)
}

/// Delete all feeds, folders and articles. Irreversible.
#[tauri::command]
pub async fn clear_all_data(app: AppHandle) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::clear_all_data(&conn)?;
    }
    let _ = app.emit("feeds-updated", 0);
    refresh_unread_surfaces(&app).await;
    Ok(())
}

// ─────────────────────────── network ───────────────────────────

/// Rebuild the HTTP client from the persisted proxy / timeout settings so the
/// change takes effect without an app restart.
#[tauri::command]
pub async fn apply_network_settings(state: State<'_, AppState>) -> AppResult<()> {
    let client = {
        // Pure settings read — use the read pool, not the writer lock.
        let conn = state.read().await;
        fetch::build_client_from_settings(&conn)
    };
    state.set_http(client);
    Ok(())
}

// ─────────────────────────── FreshRSS sync ───────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshRssStatus {
    connected: bool,
    url: Option<String>,
    /// Which GReader-compatible backend is connected: "freshrss" or
    /// "miniflux". Always present (defaults to "freshrss") so the UI never
    /// has to guess for older installs.
    provider: String,
}

#[tauri::command]
pub async fn freshrss_connect(
    app: AppHandle,
    url: String,
    username: String,
    password: String,
    provider: Option<String>,
) -> AppResult<()> {
    crate::sync::connect(&app, &url, &username, &password, provider.as_deref()).await
}

#[tauri::command]
pub async fn freshrss_disconnect(app: AppHandle) -> AppResult<()> {
    crate::sync::disconnect(&app).await
}

#[tauri::command]
pub async fn freshrss_status(app: AppHandle) -> AppResult<FreshRssStatus> {
    let info = crate::sync::connected_url(&app).await?;
    let (url, provider) = match info {
        Some((u, p)) => (Some(u), p),
        None => (None, "freshrss".to_string()),
    };
    Ok(FreshRssStatus {
        connected: url.is_some(),
        url,
        provider,
    })
}

/// Run a full FreshRSS sync now; returns the number of reconciled articles.
#[tauri::command]
pub async fn freshrss_sync(app: AppHandle) -> AppResult<usize> {
    let n = crate::sync::sync_now(&app).await?;
    let _ = app.emit("feeds-updated", 0);
    refresh_unread_surfaces(&app).await;
    Ok(n)
}

/// Rebuild the tray menu — used after a language change.
#[tauri::command]
pub async fn refresh_tray(app: AppHandle) -> AppResult<()> {
    crate::tray::refresh(&app).await;
    Ok(())
}

/// Drain a `papr://subscribe` URL that was delivered before the webview could
/// receive the `deep-link-subscribe` event (a cold-start launch). The frontend
/// calls this once on mount; returns `None` when there is nothing pending.
#[tauri::command]
pub async fn take_pending_deep_link(state: State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(state.take_pending_deep_link())
}

// ─────────────────────────── tags ───────────────────────────

#[tauri::command]
pub async fn list_tags(state: State<'_, AppState>) -> AppResult<Vec<Tag>> {
    let conn = state.read().await;
    db::list_tags(&conn)
}

#[tauri::command]
pub async fn create_tag(state: State<'_, AppState>, name: String) -> AppResult<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::code("emptyTagName"));
    }
    let conn = state.db.lock().await;
    db::create_tag(&conn, name)
}

#[tauri::command]
pub async fn rename_tag(state: State<'_, AppState>, id: i64, name: String) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::code("emptyTagName"));
    }
    let conn = state.db.lock().await;
    db::rename_tag(&conn, id, name)
}

#[tauri::command]
pub async fn set_tag_color(
    state: State<'_, AppState>,
    id: i64,
    color: String,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::set_tag_color(&conn, id, &color)
}

#[tauri::command]
pub async fn delete_tag(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_tag(&conn, id)
}

/// Attach or detach a tag from one article.
#[tauri::command]
pub async fn set_article_tag(
    state: State<'_, AppState>,
    article_id: i64,
    tag_id: i64,
    on: bool,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::set_article_tag(&conn, article_id, tag_id, on)
}

// ─────────────────────────── filter rules ───────────────────────────

#[tauri::command]
pub async fn list_rules(state: State<'_, AppState>) -> AppResult<Vec<Rule>> {
    let conn = state.read().await;
    db::list_rules(&conn)
}

#[tauri::command]
pub async fn create_rule(
    state: State<'_, AppState>,
    name: String,
    feed_id: Option<i64>,
    field: String,
    query: String,
    action: String,
) -> AppResult<i64> {
    if query.trim().is_empty() {
        return Err(AppError::code("emptyRuleQuery"));
    }
    let conn = state.db.lock().await;
    db::create_rule(&conn, name.trim(), feed_id, &field, query.trim(), &action)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn update_rule(
    state: State<'_, AppState>,
    id: i64,
    name: String,
    enabled: bool,
    feed_id: Option<i64>,
    field: String,
    query: String,
    action: String,
) -> AppResult<()> {
    if query.trim().is_empty() {
        return Err(AppError::code("emptyRuleQuery"));
    }
    let conn = state.db.lock().await;
    db::update_rule(&conn, id, name.trim(), enabled, feed_id, &field, query.trim(), &action)
}

#[tauri::command]
pub async fn delete_rule(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_rule(&conn, id)
}

/// Persist a reordered tag list (ids in the new display order).
#[tauri::command]
pub async fn reorder_tags(state: State<'_, AppState>, ids: Vec<i64>) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::reorder_tags(&conn, &ids)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RulePreview {
    count: i64,
    samples: Vec<String>,
}

/// Dry-run a draft rule against already-stored articles.
#[tauri::command]
pub async fn preview_rule(
    state: State<'_, AppState>,
    feed_id: Option<i64>,
    field: String,
    query: String,
) -> AppResult<RulePreview> {
    let conn = state.read().await;
    let (count, samples) = db::preview_rule(&conn, feed_id, &field, query.trim())?;
    Ok(RulePreview { count, samples })
}

// ─────────────────────────── newsletter sources ───────────────────────────

/// A configured email-newsletter source, as shown in the UI (no password).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterSource {
    feed_id: i64,
    title: String,
    host: String,
    port: u16,
    username: String,
    folder: String,
}

/// Payload for `add_newsletter_source` — the IMAP mailbox to start polling.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsletterInput {
    /// A display name for the source (falls back to the username).
    pub title: Option<String>,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// IMAP app-password / token. Stored in the local DB only.
    pub password: String,
    /// Mailbox to poll, e.g. `INBOX` or `Newsletters`.
    pub folder: String,
}

/// Add an email-newsletter source. Verifies the IMAP credentials by polling
/// the mailbox once, ingests whatever it finds, and persists the source so the
/// background scheduler keeps polling it. Backed by a `feeds` row plus an
/// entry in `newsletter_sources` (see migration #10).
#[tauri::command]
pub async fn add_newsletter_source(
    state: State<'_, AppState>,
    input: NewsletterInput,
) -> AppResult<Feed> {
    let cfg = NewsletterConfig {
        host: input.host.trim().to_string(),
        port: input.port,
        username: input.username.trim().to_string(),
        password: input.password.clone(),
        folder: {
            let f = input.folder.trim();
            if f.is_empty() { "INBOX".to_string() } else { f.to_string() }
        },
    };
    if cfg.host.is_empty() || cfg.username.is_empty() || cfg.password.is_empty() {
        return Err(AppError::code("newsletterMissingFields"));
    }
    let feed_url = newsletter::synthetic_feed_url(&cfg);
    let title = input
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .unwrap_or_else(|| cfg.username.clone());

    // Reject a duplicate mailbox before doing the (slow) IMAP round-trip.
    {
        let conn = state.read().await;
        if db::find_feed_by_url(&conn, &feed_url)?.is_some() {
            return Err(AppError::code("alreadySubscribed"));
        }
    }

    // Verify the credentials by actually connecting. The `imap` crate is
    // blocking, so the connection runs on the blocking pool — and it has no
    // per-operation timeout, so a server that completes the TCP/TLS handshake
    // but then stalls mid-command would block this command forever (the Add
    // dialog spinner never resolves, the blocking worker thread is leaked).
    // Bound the whole probe with the same wall-clock cap the scheduler's
    // background poll uses, so a wedged mailbox degrades to a clean error.
    let probe_cfg = cfg.clone();
    let messages = match tokio::time::timeout(
        std::time::Duration::from_secs(scheduler::NEWSLETTER_POLL_TIMEOUT_SECS),
        tokio::task::spawn_blocking(move || newsletter::fetch_recent(&probe_cfg, 30)),
    )
    .await
    {
        Ok(joined) => joined
            .map_err(|e| AppError::other(format!("newsletter poll task: {e}")))??,
        Err(_) => return Err(AppError::code("newsletterPollTimeout")),
    };

    // Persist the source, then ingest the messages just fetched.
    let conn = state.db.lock().await;
    let feed_id = db::insert_newsletter_source(&conn, &feed_url, &title, &cfg)?;
    let rules = db::active_rules(&conn).unwrap_or_default();
    // `upsert_article` returns `true` only for genuinely new *unread* rows, so
    // articles a `read` rule pre-marked read are correctly excluded from the
    // returned `unread_count` (matching the sidebar's `list_feeds` count).
    let mut unread = 0i64;
    for raw in &messages {
        if let Some(parsed) = newsletter::email_to_article(raw) {
            if db::upsert_article(&conn, feed_id, &parsed.article, false, &rules)? {
                unread += 1;
            }
        }
    }
    // Record that the mailbox was just polled. The IMAP fetch above is a
    // genuine, successful refresh of this source — without this the feed's
    // `last_fetched_at` stays NULL and the sidebar reads it as "never
    // refreshed" until the next scheduler tick (up to the refresh interval
    // away). Mirrors `touch_feed` in `scheduler::poll_newsletters` for the
    // background poll, and the same handling `add_feed` applies.
    let _ = db::touch_feed(&conn, feed_id);
    let last_fetched_at = db::feed_last_fetched(&conn, feed_id).ok().flatten();
    drop(conn);

    Ok(Feed {
        id: feed_id,
        feed_url,
        site_url: None,
        title,
        description: None,
        favicon_url: None,
        folder_id: None,
        source_type: SourceType::Newsletter.as_str().to_string(),
        last_fetched_at,
        fetch_error: None,
        unread_count: unread,
    })
}

/// Every configured newsletter source (passwords omitted).
#[tauri::command]
pub async fn list_newsletter_sources(
    state: State<'_, AppState>,
) -> AppResult<Vec<NewsletterSource>> {
    let conn = state.read().await;
    Ok(db::list_newsletter_sources(&conn)?
        .into_iter()
        .map(|r| NewsletterSource {
            feed_id: r.feed_id,
            title: r.title,
            host: r.host,
            port: r.port,
            username: r.username,
            folder: r.folder,
        })
        .collect())
}

/// Remove a newsletter source and all of its ingested articles.
#[tauri::command]
pub async fn remove_newsletter_source(
    state: State<'_, AppState>,
    feed_id: i64,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_newsletter_source(&conn, feed_id)
}

// ─────────────────────────── highlights (F7) ───────────────────────────

/// Create a highlight on an article. The frontend supplies the quote plus its
/// anchoring context (prefix / suffix window and the plain-text offset).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn create_highlight(
    state: State<'_, AppState>,
    article_id: i64,
    quote: String,
    prefix: String,
    suffix: String,
    text_offset: i64,
    color: String,
    note: String,
) -> AppResult<i64> {
    if quote.trim().is_empty() {
        return Err(AppError::code("emptyHighlight"));
    }
    let conn = state.db.lock().await;
    db::insert_highlight(
        &conn,
        &db::NewHighlight {
            article_id,
            quote: &quote,
            prefix: &prefix,
            suffix: &suffix,
            text_offset,
            color: &color,
            note: &note,
        },
    )
}

/// Every highlight on one article, in reading order.
#[tauri::command]
pub async fn list_highlights(
    state: State<'_, AppState>,
    article_id: i64,
) -> AppResult<Vec<Highlight>> {
    let conn = state.read().await;
    db::list_highlights(&conn, article_id)
}

/// Every highlight across all articles — for the Highlights browser.
#[tauri::command]
pub async fn list_all_highlights(state: State<'_, AppState>) -> AppResult<Vec<Highlight>> {
    let conn = state.read().await;
    db::list_all_highlights(&conn)
}

/// Replace a highlight's note (an empty string clears it).
#[tauri::command]
pub async fn update_highlight_note(
    state: State<'_, AppState>,
    id: i64,
    note: String,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::update_highlight_note(&conn, id, &note)
}

/// Change a highlight's colour (a palette key).
#[tauri::command]
pub async fn set_highlight_color(
    state: State<'_, AppState>,
    id: i64,
    color: String,
) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::set_highlight_color(&conn, id, &color)
}

#[tauri::command]
pub async fn delete_highlight(state: State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = state.db.lock().await;
    db::delete_highlight(&conn, id)
}

// ─────────────────────────── highlight export (F7) ───────────────────────────

/// Read a required, non-empty setting, returning the localisable `missing`
/// error code when the key is absent or holds only whitespace.
fn require_setting(
    conn: &rusqlite::Connection,
    key: &str,
    missing: &'static str,
) -> AppResult<String> {
    db::get_setting(conn, key)?
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::code(missing))
}

/// Gather an article and its highlights from the database for export.
fn load_for_export(
    conn: &rusqlite::Connection,
    article_id: i64,
) -> AppResult<(ExportArticle, Vec<Highlight>)> {
    let detail = db::get_article(conn, article_id)?;
    let article = ExportArticle {
        title: detail.title,
        url: detail.url,
        author: detail.author,
        feed_title: detail.feed_title,
        published_at: detail.published_at,
    };
    let highlights = db::list_highlights(conn, article_id)?;
    Ok((article, highlights))
}

/// Render an article's highlights as a Markdown document — used for both
/// copy-to-clipboard (frontend) and save-to-file.
#[tauri::command]
pub async fn export_highlights_markdown(
    state: State<'_, AppState>,
    article_id: i64,
) -> AppResult<String> {
    let conn = state.read().await;
    let (article, highlights) = load_for_export(&conn, article_id)?;
    Ok(export::build_markdown(&article, &highlights))
}

/// Derive a filesystem-safe `.md` note name from an article title, falling
/// back to a stable `highlights-<id>.md` when the title yields an empty stem.
fn obsidian_note_filename(title: &str, article_id: i64) -> String {
    export::safe_filename(title, ".md", &format!("highlights-{article_id}.md"))
}

/// Write an article's highlights as a Markdown file into the user-configured
/// Obsidian vault folder (the `obsidian_vault` setting). Returns the path
/// written. Uses `std::fs` directly — no file-dialog plugin is bundled.
#[tauri::command]
pub async fn export_highlights_to_obsidian(
    state: State<'_, AppState>,
    article_id: i64,
) -> AppResult<String> {
    let (markdown, vault, title) = {
        let conn = state.read().await;
        let (article, highlights) = load_for_export(&conn, article_id)?;
        let vault = require_setting(&conn, "obsidian_vault", "noObsidianVault")?;
        let markdown = export::build_markdown(&article, &highlights);
        (markdown, vault, article.title)
    };
    let name = obsidian_note_filename(&title, article_id);
    let dir = PathBuf::from(vault.trim());
    fs::create_dir_all(&dir)
        .map_err(|e| AppError::other(format!("create vault folder: {e}")))?;
    let path = dir.join(name);
    fs::write(&path, markdown)
        .map_err(|e| AppError::other(format!("write note: {e}")))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Push an article's highlights to Readwise. The access token is read from
/// the `readwise_token` setting.
#[tauri::command]
pub async fn export_highlights_to_readwise(
    state: State<'_, AppState>,
    article_id: i64,
) -> AppResult<usize> {
    let (article, highlights, token) = {
        let conn = state.read().await;
        let (article, highlights) = load_for_export(&conn, article_id)?;
        let token = require_setting(&conn, "readwise_token", "noReadwiseToken")?;
        (article, highlights, token)
    };
    if highlights.is_empty() {
        return Err(AppError::code("noHighlights"));
    }
    let count = highlights.len();
    let http = state.http();
    export::post_to_readwise(&http, token.trim(), &article, &highlights).await?;
    Ok(count)
}

/// Append an article's highlights to a Notion page. The integration token and
/// target page id are read from the `notion_token` / `notion_page` settings.
#[tauri::command]
pub async fn export_highlights_to_notion(
    state: State<'_, AppState>,
    article_id: i64,
) -> AppResult<usize> {
    let (article, highlights, token, page) = {
        let conn = state.read().await;
        let (article, highlights) = load_for_export(&conn, article_id)?;
        let token = require_setting(&conn, "notion_token", "noNotionToken")?;
        let page = require_setting(&conn, "notion_page", "noNotionPage")?;
        (article, highlights, token, page)
    };
    if highlights.is_empty() {
        return Err(AppError::code("noHighlights"));
    }
    let count = highlights.len();
    let http = state.http();
    export::post_to_notion(&http, token.trim(), page.trim(), &article, &highlights).await?;
    Ok(count)
}

// ─────────────────────────── "Send to…" share (F8) ───────────────────────────

/// The four "Send to…" targets. Mirrored by the `ShareTarget` union in
/// `src/types.ts`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareTarget {
    Pocket,
    Instapaper,
    Kindle,
    Notion,
}

/// Strip HTML tags from a body, collapsing each block element to a newline so
/// the plain text keeps paragraph breaks. Good enough for a Notion page body.
fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    // Lower-cased tag name accumulated while inside `<...>`.
    let mut tag = String::new();
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                // Block-level tags become a line break.
                let name = tag.trim_start_matches('/');
                if matches!(
                    name.split([' ', '\t', '\n']).next().unwrap_or(""),
                    "p" | "br" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5"
                        | "h6" | "tr" | "blockquote" | "section" | "article"
                ) {
                    out.push('\n');
                }
            }
            _ if in_tag => tag.push(c.to_ascii_lowercase()),
            _ => out.push(c),
        }
    }
    // Decode the handful of entities a sanitized body can carry. `&amp;` is
    // decoded *last*, after every other entity: doing it first would
    // double-decode an escaped entity — a body that literally shows the text
    // `&lt;` is encoded by `sanitize` as `&amp;lt;`, and an `&amp;`-first pass
    // would turn that into `&lt;` and then `<`, corrupting the visible text.
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// Load an article from the database into a `ShareArticle`. The body prefers
/// the extracted full text, falling back to the feed-supplied content.
fn load_for_share(
    conn: &rusqlite::Connection,
    article_id: i64,
) -> AppResult<ShareArticle> {
    let detail = db::get_article(conn, article_id)?;
    let body_html = detail
        .extracted_html
        .filter(|h| !h.trim().is_empty())
        .or(detail.content_html)
        .unwrap_or_default();
    Ok(ShareArticle {
        title: detail.title,
        url: detail.url,
        author: detail.author,
        feed_title: detail.feed_title,
        published_at: detail.published_at,
        body_html,
    })
}

/// Which "Send to…" targets currently have complete credentials configured.
/// The UI uses this to only show usable targets.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareTargets {
    pocket: bool,
    instapaper: bool,
    kindle: bool,
    notion: bool,
}

/// Report which share targets are configured (have all required settings).
#[tauri::command]
pub async fn share_targets(state: State<'_, AppState>) -> AppResult<ShareTargets> {
    let conn = state.read().await;
    let set = |key: &str| -> bool {
        db::get_setting(&conn, key)
            .ok()
            .flatten()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    Ok(ShareTargets {
        pocket: set("pocket_consumer_key") && set("pocket_access_token"),
        instapaper: set("instapaper_username") && set("instapaper_password"),
        kindle: set("kindle_smtp_host")
            && set("kindle_smtp_username")
            && set("kindle_smtp_password")
            && set("kindle_address"),
        notion: set("notion_token") && set("notion_page"),
    })
}

/// Send one article to a read-later / archive / note service (feature F8).
/// Loads the article, builds the target-specific payload, and performs the
/// network or SMTP call. Returns an `AppError` with a localisable code when
/// the chosen target is not configured.
#[tauri::command]
pub async fn send_article(
    state: State<'_, AppState>,
    article_id: i64,
    target: ShareTarget,
) -> AppResult<()> {
    match target {
        ShareTarget::Pocket => {
            let (article, key, token) = {
                let conn = state.read().await;
                let article = load_for_share(&conn, article_id)?;
                let key = require_setting(&conn, "pocket_consumer_key", "noPocketConfig")?;
                let token =
                    require_setting(&conn, "pocket_access_token", "noPocketConfig")?;
                (article, key, token)
            };
            let http = state.http();
            share::post_to_pocket(&http, key.trim(), token.trim(), &article).await
        }
        ShareTarget::Instapaper => {
            let (article, user, pass) = {
                let conn = state.read().await;
                let article = load_for_share(&conn, article_id)?;
                let user =
                    require_setting(&conn, "instapaper_username", "noInstapaperConfig")?;
                let pass =
                    require_setting(&conn, "instapaper_password", "noInstapaperConfig")?;
                (article, user, pass)
            };
            let http = state.http();
            share::post_to_instapaper(&http, user.trim(), pass.trim(), &article, None).await
        }
        ShareTarget::Kindle => {
            let (article, cfg) = {
                let conn = state.read().await;
                let article = load_for_share(&conn, article_id)?;
                let cfg = KindleConfig::new(
                    db::get_setting(&conn, "kindle_smtp_host")?,
                    db::get_setting(&conn, "kindle_smtp_port")?,
                    db::get_setting(&conn, "kindle_smtp_username")?,
                    db::get_setting(&conn, "kindle_smtp_password")?,
                    db::get_setting(&conn, "kindle_from_address")?,
                    db::get_setting(&conn, "kindle_address")?,
                )?;
                (article, cfg)
            };
            // `lettre`'s SMTP transport is blocking — run it off the async pool.
            tokio::task::spawn_blocking(move || share::send_to_kindle(&cfg, &article))
                .await
                .map_err(|e| AppError::other(format!("kindle send task: {e}")))?
        }
        ShareTarget::Notion => {
            let (export_article, body_text, token, page) = {
                let conn = state.read().await;
                let article = load_for_share(&conn, article_id)?;
                let body_text = html_to_text(&article.body_html);
                let export_article = ExportArticle {
                    title: article.title,
                    url: article.url,
                    author: article.author,
                    feed_title: article.feed_title,
                    published_at: article.published_at,
                };
                let token = require_setting(&conn, "notion_token", "noNotionToken")?;
                let page = require_setting(&conn, "notion_page", "noNotionPage")?;
                (export_article, body_text, token, page)
            };
            let http = state.http();
            export::post_article_to_notion(
                &http,
                token.trim(),
                page.trim(),
                &export_article,
                &body_text,
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{html_to_text, obsidian_note_filename};

    #[test]
    fn obsidian_filename_replaces_reserved_chars() {
        assert_eq!(
            obsidian_note_filename("a/b:c*d?", 1),
            "a-b-c-d-.md"
        );
    }

    #[test]
    fn obsidian_filename_falls_back_for_empty_or_dotted_title() {
        // Whitespace-only, and titles that would yield a hidden / odd file.
        assert_eq!(obsidian_note_filename("   ", 7), "highlights-7.md");
        assert_eq!(obsidian_note_filename(".", 7), "highlights-7.md");
        assert_eq!(obsidian_note_filename("..", 7), "highlights-7.md");
    }

    #[test]
    fn obsidian_filename_strips_leading_dot() {
        // A leading dot would make the note a hidden file in the vault.
        assert_eq!(obsidian_note_filename(".hidden", 1), "hidden.md");
    }

    #[test]
    fn obsidian_filename_truncates_to_byte_limit() {
        // A title longer than the 255-byte component cap must not produce a
        // filename that fails `fs::write` with ENAMETOOLONG.
        let long = "a".repeat(400);
        let name = obsidian_note_filename(&long, 1);
        assert!(name.len() <= 255, "filename {} bytes", name.len());
        assert!(name.ends_with(".md"));
    }

    #[test]
    fn obsidian_filename_truncates_on_char_boundary() {
        // CJL characters are 3 bytes each — truncation must not split one.
        let long = "界".repeat(200); // 600 bytes
        let name = obsidian_note_filename(&long, 1);
        assert!(name.len() <= 255);
        assert!(name.ends_with(".md"));
        // The stem (sans ".md") must still be valid UTF-8 whole characters.
        let stem = name.strip_suffix(".md").unwrap();
        assert!(stem.chars().all(|c| c == '界'));
    }

    #[test]
    fn obsidian_filename_keeps_ordinary_title() {
        assert_eq!(
            obsidian_note_filename("My Article Title", 1),
            "My Article Title.md"
        );
    }

    #[test]
    fn html_to_text_strips_tags() {
        assert_eq!(html_to_text("<p>hello</p>").trim(), "hello");
        assert_eq!(html_to_text("plain").trim(), "plain");
    }

    #[test]
    fn html_to_text_block_tags_become_newlines() {
        let t = html_to_text("<p>one</p><p>two</p>");
        let lines: Vec<&str> = t.split('\n').filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines, vec!["one", "two"]);
        assert_eq!(html_to_text("a<br>b").replace('\n', "|"), "a|b");
    }

    #[test]
    fn html_to_text_inline_tags_kept_inline() {
        assert_eq!(
            html_to_text("<p>a <strong>bold</strong> word</p>").trim(),
            "a bold word"
        );
    }

    #[test]
    fn html_to_text_decodes_entities() {
        assert_eq!(
            html_to_text("<p>tom &amp; jerry &lt;3 &quot;x&quot;</p>").trim(),
            "tom & jerry <3 \"x\""
        );
    }

    #[test]
    fn html_to_text_does_not_double_decode_escaped_entities() {
        // `sanitize` encodes a body that literally shows the text `&lt;` as
        // `&amp;lt;`. Decoding `&amp;` before `&lt;` would turn that into
        // `&lt;` and then `<`, corrupting the visible text. `&amp;` must be
        // decoded last so each entity is decoded exactly once.
        assert_eq!(
            html_to_text("<p>write &amp;lt;br&amp;gt; for a break</p>").trim(),
            "write &lt;br&gt; for a break"
        );
        // `&amp;amp;` is the escaped form of the literal text `&amp;`.
        assert_eq!(html_to_text("<p>&amp;amp;</p>").trim(), "&amp;");
    }

    #[test]
    fn html_to_text_empty_input() {
        assert_eq!(html_to_text("").trim(), "");
        assert_eq!(html_to_text("<p></p>").trim(), "");
    }

    #[test]
    fn html_to_text_handles_attributes_in_tags() {
        // A tag carrying attributes is still recognised as block-level.
        let t = html_to_text("<p class=\"x\">first</p><div id=\"y\">second</div>");
        let lines: Vec<&str> = t.split('\n').filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines, vec!["first", "second"]);
    }
}
