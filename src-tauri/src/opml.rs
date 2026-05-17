//! OPML import/export — the portable subscription-list format every reader
//! supports, so users can migrate in from (and out to) Reeder, Feedly, etc.

use crate::error::{AppError, AppResult};
use opml::{Head, Outline, OPML};
use std::collections::BTreeMap;

/// A subscription parsed out of an OPML file.
pub struct ImportedFeed {
    pub feed_url: String,
    pub title: String,
    pub folder: Option<String>,
}

/// Parse an OPML document into a flat list of feeds with their folder names.
pub fn parse(content: &str) -> AppResult<Vec<ImportedFeed>> {
    let doc = OPML::from_str(content).map_err(|e| AppError::Opml(e.to_string()))?;
    let mut feeds = Vec::new();
    for outline in &doc.body.outlines {
        collect(outline, None, &mut feeds);
    }
    Ok(feeds)
}

fn collect(outline: &Outline, folder: Option<&str>, out: &mut Vec<ImportedFeed>) {
    if let Some(url) = &outline.xml_url {
        let title = if outline.text.is_empty() {
            outline.title.clone().unwrap_or_else(|| url.clone())
        } else {
            outline.text.clone()
        };
        out.push(ImportedFeed {
            feed_url: url.clone(),
            title,
            folder: folder.map(|s| s.to_string()),
        });
    }
    // A childless-of-feeds outline acts as a folder for its descendants.
    let child_folder = if outline.xml_url.is_none() && !outline.outlines.is_empty() {
        Some(outline.text.as_str())
    } else {
        folder
    };
    for child in &outline.outlines {
        collect(child, child_folder, out);
    }
}

/// Build an OPML document from `(title, feed_url, folder)` tuples.
pub fn build(feeds: &[(String, String, Option<String>)]) -> AppResult<String> {
    let mut doc = OPML {
        head: Some(Head {
            title: Some("Papr Subscriptions".to_string()),
            ..Head::default()
        }),
        ..OPML::default()
    };

    let mut by_folder: BTreeMap<Option<String>, Vec<Outline>> = BTreeMap::new();
    for (title, url, folder) in feeds {
        let outline = Outline {
            text: title.clone(),
            xml_url: Some(url.clone()),
            r#type: Some("rss".to_string()),
            ..Outline::default()
        };
        by_folder.entry(folder.clone()).or_default().push(outline);
    }

    for (folder, outlines) in by_folder {
        match folder {
            Some(name) => doc.body.outlines.push(Outline {
                text: name,
                outlines,
                ..Outline::default()
            }),
            None => doc.body.outlines.extend(outlines),
        }
    }

    doc.to_string().map_err(|e| AppError::Opml(e.to_string()))
}
