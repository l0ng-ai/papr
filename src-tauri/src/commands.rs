//! Tauri command surface — the typed IPC boundary the React frontend calls.
//! All SQL is delegated to `db`; commands only orchestrate.

use crate::ai::{self, AiConfig, AiEvent};
use crate::db::{self};
use crate::error::{AppError, AppResult};
use crate::extraction;
use crate::ingestion::{fetch, parse, scheduler};
use crate::models::*;
use crate::opml;
use crate::state::AppState;
use serde::Serialize;
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
/// case we auto-discover the feed). Fetches once so the feed is immediately
/// populated, then returns the stored feed.
#[tauri::command]
pub async fn add_feed(
    state: State<'_, AppState>,
    url: String,
    folder_id: Option<i64>,
) -> AppResult<Feed> {
    let client = state.http();

    // Step 1: fetch whatever the user gave us.
    let (bytes, _ct, final_url) = fetch::get(&client, &url).await?;

    // Step 2: if it is a feed use it directly, otherwise discover one.
    let (feed_url, feed_bytes) = if parse::looks_like_feed(&bytes) {
        (final_url, bytes)
    } else {
        let html = String::from_utf8_lossy(&bytes);
        let candidates = parse::discover_feeds(&html, &final_url);
        let candidate = candidates
            .into_iter()
            .next()
            .ok_or_else(|| AppError::code("noFeedFound"))?;
        let (fb, _, _) = fetch::get(&client, &candidate).await?;
        (candidate, fb)
    };

    // Step 3: parse and classify.
    let parsed = parse::parse_feed(&feed_bytes, &feed_url)?;
    let source_type =
        parse::refine_source_type(parse::detect_source_type(&feed_url), &parsed, &feed_url);

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
    let dedup = db::get_setting(&conn, "dedup_enabled")?
        .map(|v| v == "1")
        .unwrap_or(false);
    let rules = db::active_rules(&conn).unwrap_or_default();
    let mut unread = 0i64;
    for article in &parsed.articles {
        if db::upsert_article(&conn, feed_id, article, dedup, &rules)? {
            unread += 1;
        }
    }
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
        last_fetched_at: None,
        fetch_error: None,
        unread_count: unread,
    })
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
    let conn = state.db.lock().await;
    db::rename_feed(&conn, id, title.trim())
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

#[tauri::command]
pub async fn mark_read(app: AppHandle, id: i64, read: bool) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        let conn = state.db.lock().await;
        db::set_read(&conn, id, read)?;
        enqueue_if_connected(&conn, id, "read", read);
    }
    crate::notify::update_badge(&app).await;
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
    crate::notify::update_badge(&app).await;
    crate::tray::refresh(&app).await;
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
    let (bytes, _ct, final_url) = fetch::get(&http, &url).await?;
    let html = String::from_utf8_lossy(&bytes).into_owned();

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
    )
}

/// Truncate to at most `max` characters without splitting a UTF-8 boundary.
fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Stream an AI summary of one article; the full summary is also persisted.
#[tauri::command]
pub async fn ai_summarize(
    state: State<'_, AppState>,
    article_id: i64,
    on_token: Channel<AiEvent>,
) -> AppResult<()> {
    let (title, body, cfg) = {
        let conn = state.read().await;
        let (title, body) = db::article_text(&conn, article_id)?;
        (title, body, load_ai_config(&conn)?)
    };
    let system = "You are a sharp news editor. Summarize the article in 3-4 \
                  clear, factual sentences. Output only the summary prose.";
    let user = format!("Title: {title}\n\n{}", truncate(&body, 8000));

    let http = state.http();
    let summary = ai::stream_chat(&http, &cfg, system, &user, &on_token).await?;
    if !summary.trim().is_empty() {
        let conn = state.db.lock().await;
        db::set_ai_summary(&conn, article_id, summary.trim())?;
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
    let (cfg, context) = {
        let conn = state.read().await;
        let cfg = load_ai_config(&conn)?;
        let hits =
            db::list_articles(&conn, &ArticleQuery::All, false, Some(&question), false, 6, 0)?;
        let mut context = String::new();
        for hit in hits {
            let (title, body) = db::article_text(&conn, hit.id)?;
            context.push_str(&format!(
                "## {} — {}\n{}\n\n",
                title,
                hit.feed_title,
                truncate(&body, 1200)
            ));
        }
        (cfg, context)
    };

    let system = "You answer the user's question using only the provided \
                  articles from their RSS subscriptions. Cite the article \
                  titles you draw from. If the articles do not contain the \
                  answer, say so plainly.";
    let user = if context.trim().is_empty() {
        format!("No relevant articles were found.\n\nQuestion: {question}")
    } else {
        format!("Articles from the user's feeds:\n\n{context}---\n\nQuestion: {question}")
    };

    let http = state.http();
    ai::stream_chat(&http, &cfg, system, &user, &on_token).await?;
    Ok(())
}

/// Stream an AI briefing that synthesizes the most recent articles by theme.
#[tauri::command]
pub async fn ai_digest(
    state: State<'_, AppState>,
    on_token: Channel<AiEvent>,
) -> AppResult<()> {
    let (cfg, articles) = {
        let conn = state.read().await;
        (load_ai_config(&conn)?, db::digest_source(&conn, 30)?)
    };
    if articles.is_empty() {
        return Err(AppError::code("noArticles"));
    }

    let mut corpus = String::new();
    for (title, feed, text) in &articles {
        corpus.push_str(&format!("- [{feed}] {title}: {}\n", truncate(text, 400)));
    }

    let system = "You are the user's personal news briefer. From the recent \
                  articles, write a crisp briefing: group related items into \
                  2-4 themed sections with short headers, lead with what \
                  matters most, and keep it skimmable. Plain prose, no preamble.";
    let user = format!("Recent articles from my feeds:\n\n{corpus}");

    let http = state.http();
    ai::stream_chat(&http, &cfg, system, &user, &on_token).await?;
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
    crate::notify::update_badge(&app).await;
    crate::tray::refresh(&app).await;
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
}

#[tauri::command]
pub async fn freshrss_connect(
    app: AppHandle,
    url: String,
    username: String,
    password: String,
) -> AppResult<()> {
    crate::sync::connect(&app, &url, &username, &password).await
}

#[tauri::command]
pub async fn freshrss_disconnect(app: AppHandle) -> AppResult<()> {
    crate::sync::disconnect(&app).await
}

#[tauri::command]
pub async fn freshrss_status(app: AppHandle) -> AppResult<FreshRssStatus> {
    let url = crate::sync::connected_url(&app).await?;
    Ok(FreshRssStatus {
        connected: url.is_some(),
        url,
    })
}

/// Run a full FreshRSS sync now; returns the number of reconciled articles.
#[tauri::command]
pub async fn freshrss_sync(app: AppHandle) -> AppResult<usize> {
    let n = crate::sync::sync_now(&app).await?;
    let _ = app.emit("feeds-updated", 0);
    crate::notify::update_badge(&app).await;
    crate::tray::refresh(&app).await;
    Ok(n)
}

/// Rebuild the tray menu — used after a language change.
#[tauri::command]
pub async fn refresh_tray(app: AppHandle) -> AppResult<()> {
    crate::tray::refresh(&app).await;
    Ok(())
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
