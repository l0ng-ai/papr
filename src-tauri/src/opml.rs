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

/// The human-facing label of an outline: its `text` attribute, falling back to
/// the `title` attribute. Many OPML exporters label folder/feed outlines with
/// only `title` (which the spec permits), leaving `text` empty.
fn outline_label(outline: &Outline) -> Option<&str> {
    if !outline.text.is_empty() {
        Some(outline.text.as_str())
    } else {
        outline.title.as_deref().filter(|t| !t.is_empty())
    }
}

fn collect(outline: &Outline, folder: Option<&str>, out: &mut Vec<ImportedFeed>) {
    if let Some(url) = &outline.xml_url {
        let title = outline_label(outline).unwrap_or(url).to_string();
        out.push(ImportedFeed {
            feed_url: url.clone(),
            title,
            folder: folder.map(|s| s.to_string()),
        });
    }
    // A childless-of-feeds outline acts as a folder for its descendants. Use
    // the same `text`-then-`title` label resolution feeds get — otherwise a
    // folder outline labelled only with `title` would import its feeds into a
    // folder with an empty name.
    let child_folder = if outline.xml_url.is_none() && !outline.outlines.is_empty() {
        outline_label(outline).or(folder)
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

#[cfg(test)]
mod tests {
    use super::{build, parse};

    #[test]
    fn parses_flat_feed_list() {
        let xml = r#"<opml version="2.0"><head/><body>
            <outline text="Blog A" xmlUrl="https://a.example/feed.xml"/>
            <outline text="Blog B" xmlUrl="https://b.example/feed.xml"/>
        </body></opml>"#;
        let feeds = parse(xml).expect("parse");
        assert_eq!(feeds.len(), 2);
        assert_eq!(feeds[0].title, "Blog A");
        assert_eq!(feeds[0].feed_url, "https://a.example/feed.xml");
        assert!(feeds[0].folder.is_none());
    }

    #[test]
    fn parses_feeds_nested_in_a_folder() {
        let xml = r#"<opml version="2.0"><head/><body>
            <outline text="Tech">
                <outline text="Feed 1" xmlUrl="https://1.example/f"/>
                <outline text="Feed 2" xmlUrl="https://2.example/f"/>
            </outline>
        </body></opml>"#;
        let feeds = parse(xml).expect("parse");
        assert_eq!(feeds.len(), 2);
        assert!(feeds.iter().all(|f| f.folder.as_deref() == Some("Tech")));
    }

    #[test]
    fn folder_label_falls_back_to_title_attribute() {
        // A folder outline labelled only with `title` (no `text`) — common in
        // real-world exports. Its feeds must land in a folder named "News",
        // not a folder with an empty name.
        let xml = r#"<opml version="2.0"><head/><body>
            <outline title="News">
                <outline text="Feed" xmlUrl="https://n.example/f"/>
            </outline>
        </body></opml>"#;
        let feeds = parse(xml).expect("parse");
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].folder.as_deref(), Some("News"));
    }

    #[test]
    fn feed_title_falls_back_to_title_then_url() {
        let xml = r#"<opml version="2.0"><head/><body>
            <outline title="Titled Feed" xmlUrl="https://t.example/f"/>
            <outline xmlUrl="https://u.example/f"/>
        </body></opml>"#;
        let feeds = parse(xml).expect("parse");
        assert_eq!(feeds[0].title, "Titled Feed");
        // No text and no title — the URL itself is the last-resort label.
        assert_eq!(feeds[1].title, "https://u.example/f");
    }

    #[test]
    fn build_round_trips_through_parse() {
        let input = vec![
            (
                "Folderless".to_string(),
                "https://x.example/f".to_string(),
                None,
            ),
            (
                "In Folder".to_string(),
                "https://y.example/f".to_string(),
                Some("Tech".to_string()),
            ),
        ];
        let xml = build(&input).expect("build");
        let feeds = parse(&xml).expect("re-parse");
        assert_eq!(feeds.len(), 2);
        let folderless = feeds
            .iter()
            .find(|f| f.feed_url == "https://x.example/f")
            .expect("folderless feed");
        assert!(folderless.folder.is_none());
        let foldered = feeds
            .iter()
            .find(|f| f.feed_url == "https://y.example/f")
            .expect("foldered feed");
        assert_eq!(foldered.title, "In Folder");
        assert_eq!(foldered.folder.as_deref(), Some("Tech"));
    }

    #[test]
    fn parse_rejects_malformed_document() {
        assert!(parse("not opml at all").is_err());
    }
}
