//! Isolate a library in a named collection. Existing root files are retained.
use super::*;

pub fn directory_url(base: &str, name: &str) -> AppResult<String> {
    let mut url = base_url(base)?;
    let name = name.trim();
    if name.is_empty() {
        return Ok(url.to_string());
    }
    if name == "."
        || name == ".."
        || name.len() > 240
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(AppError::code("webdavInvalidDirectory"));
    }
    url.path_segments_mut()
        .map_err(|_| AppError::code("webdavInvalidDirectory"))?
        .pop_if_empty()
        .push(name)
        .push("");
    Ok(url.to_string())
}
fn same_root(a: &Root, b: &Root) -> bool {
    match (a, b) {
        (Root::Manifest(a), Root::Manifest(b)) => a == b,
        (Root::Legacy(a), Root::Legacy(b)) => a.version == b.version && a.records == b.records,
        _ => false,
    }
}
async fn copy(
    http: &Client,
    source: &Credentials,
    destination: &Url,
    object: &str,
    etag: Option<&str>,
) -> AppResult<reqwest::Response> {
    transport::retry(&source.url, || async {
        let mut req = request(
            http,
            source,
            reqwest::Method::from_bytes(b"COPY").unwrap(),
            source.url.join(object)?,
        )
        .header("Destination", destination.join(object)?.as_str())
        .header("Overwrite", "F");
        if let Some(etag) = etag {
            req = req.header(header::IF_MATCH, etag);
        }
        let response = req.send().await?;
        if matches!(response.status().as_u16(), 412 | 405 | 501) {
            return Ok(response);
        }
        let response = transport::checked(response).await?;
        if matches!(response.status().as_u16(), 202 | 207) {
            transport::drain(response).await;
            return Err(AppError::code("webdavInvalidData").into());
        }
        Ok(response)
    })
    .await
}
/// Server-side COPY avoids transferring an existing 1.3 GB library through the
/// user's proxy a second time. Only publish the destination root after all its
/// chunks exist. Never MOVE/delete source objects or overwrite another library.
pub async fn relocate(db: &Db, http: &Client, name: &str) -> AppResult<String> {
    let _guard = SYNC_LOCK.lock().await;
    if name.trim().is_empty() {
        return Err(AppError::code("webdavInvalidDirectory"));
    }
    let source =
        credentials(&*db.lock().await)?.ok_or_else(|| AppError::code("webdavNotConnected"))?;
    let mut destination = source.clone();
    destination.url = base_url(&directory_url(source.url.as_str(), name)?)?;
    let result = progress::run(relocate_inner(db, http, &source, &destination)).await;
    if result.is_ok() {
        progress::phase("relocated", 0);
    }
    result.map(|()| destination.url.to_string())
}
async fn relocate_inner(
    db: &Db,
    http: &Client,
    source: &Credentials,
    destination: &Credentials,
) -> AppResult<()> {
    ensure_collection(http, destination).await?;
    let target = upload_target(destination);
    for _ in 0..3 {
        let (root, etag) = download(http, source).await?;
        let (existing, target_etag) = download(http, destination).await?;
        if target_etag.is_some() {
            if same_root(&root, &existing) {
                break;
            }
            return Err(AppError::code("webdavDirectoryOccupied"));
        }
        if etag.is_none() {
            break;
        }
        if let Root::Manifest(m) = &root {
            progress::phase("relocating", m.chunks.len());
            let memory = tokio::sync::Semaphore::new(MEMORY_KIB);
            let mut work = stream::iter(m.chunks.clone())
                .map(|part| {
                    let target = &target;
                    let memory = &memory;
                    async move {
                        let _permit = memory
                            .acquire_many(memory_cost(&part))
                            .await
                            .expect("sync memory semaphore");
                        if receipt_valid(db, http, destination, &part, target).await? {
                            progress::completed();
                            return Ok(());
                        }
                        let response =
                            copy(http, source, &destination.url, &chunks::path(&part), None)
                                .await?;
                        match response.status().as_u16() {
                            405 | 501 => {
                                transport::drain(response).await;
                                if chunks::cached(&*db.lock().await, &part)?.is_none() {
                                    let body = fetch_chunk(http, source, &part).await?;
                                    chunks::cache(&*db.lock().await, &part, &body)?;
                                }
                                upload_chunk(db, http, destination, &part, target).await?;
                            }
                            412 => {
                                transport::drain(response).await;
                                let (body, etag) =
                                    fetch_chunk_meta(http, destination, &part).await?;
                                chunks::cache(&*db.lock().await, &part, &body)?;
                                save_receipt(db, target, &part, etag).await?;
                            }
                            _ => {
                                let etag = transport::etag(&response);
                                transport::drain(response).await;
                                save_receipt(db, target, &part, etag).await?;
                            }
                        }
                        progress::completed();
                        Ok::<_, AppError>(())
                    }
                })
                .buffer_unordered(source.options.concurrency);
            while let Some(result) = work.next().await {
                result?;
            }
        }
        progress::phase("publishing", 0);
        let response = copy(http, source, &destination.url, FILE, etag.as_deref()).await?;
        let status = response.status();
        transport::drain(response).await;
        if status == StatusCode::PRECONDITION_FAILED {
            let (current, created) = download(http, destination).await?;
            if created.is_some() {
                if same_root(&root, &current) {
                    break;
                }
                return Err(AppError::code("webdavDirectoryOccupied"));
            }
            continue; // source changed: copy the newer complete snapshot
        }
        if matches!(status.as_u16(), 405 | 501) {
            // Standard PUT fallback for current manifests. Legacy whole-file
            // roots require server COPY; leave the old connection unchanged.
            let Root::Manifest(m) = &root else {
                return Err(AppError::code("webdavCopyUnsupported"));
            };
            let (_, latest) = download(http, source).await?;
            if latest != etag {
                continue;
            }
            let body = serde_json::to_vec(m).map_err(|_| AppError::code("webdavInvalidData"))?;
            let response = transport::retry(&destination.url, || async {
                let resp = request(
                    http,
                    destination,
                    reqwest::Method::PUT,
                    destination.url.join(FILE)?,
                )
                .header(header::IF_NONE_MATCH, "*")
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.clone())
                .send()
                .await?;
                if resp.status() == StatusCode::PRECONDITION_FAILED {
                    return Ok(resp);
                }
                transport::checked(resp).await
            })
            .await?;
            transport::drain(response).await;
        }
        let (verified, published) = download(http, destination).await?;
        if published.is_none() || !same_root(&root, &verified) {
            return Err(AppError::code("webdavInvalidData"));
        }
        break;
    }
    let (_, ready) = download(http, destination).await?;
    let (_, source_root) = download(http, source).await?;
    if ready.is_none() && source_root.is_some() {
        return Err(AppError::code("webdavConflict"));
    }
    let mut conn = db.lock().await;
    let tx = conn.transaction()?;
    db::set_setting(&tx, "webdav_url", destination.url.as_str())?;
    tx.execute(
        "DELETE FROM webdav_upload_receipts WHERE target=?1",
        [target],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_directory_cannot_escape_the_selected_collection() {
        assert_eq!(
            directory_url("https://example.test/dav/", "Papr").unwrap(),
            "https://example.test/dav/Papr/"
        );
        assert_eq!(
            directory_url("https://example.test/dav/", "我的书库").unwrap(),
            "https://example.test/dav/%E6%88%91%E7%9A%84%E4%B9%A6%E5%BA%93/"
        );
        for name in ["..", ".", "a/b", "a\\b", "bad\nfolder"] {
            assert!(directory_url("https://example.test/dav/", name).is_err());
        }
        assert_eq!(
            directory_url("https://example.test/dav/", "a?#").unwrap(),
            "https://example.test/dav/a%3F%23/"
        );
    }
}
