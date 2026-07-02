//! TOON output boundary.
//!
//! Handlers assemble a `serde_json::Value` (internal logic stays on JSON), and
//! this module renders it to spec-correct [TOON](https://toonformat.dev) via the
//! official `toon-format` encoder — so quoting, escaping, number/boolean
//! disambiguation and tabular-array layout all come from the reference
//! implementation rather than hand-rolled string formatting.
//!
//! [`Doc`] is a thin insertion-ordered object builder: with serde_json's
//! `preserve_order` feature, fields appear in the TOON in the exact order each
//! handler sets them.

use serde_json::{Map, Value};

/// Encode a value tree as TOON for stdout, always terminated by a single
/// newline so `print!` output ends cleanly. Encoding only fails on shapes we
/// never build (e.g. non-string map keys); if it ever does, degrade to a
/// structured error line rather than panicking.
pub fn render(value: &Value) -> String {
    match toon_format::encode(value, &toon_format::EncodeOptions::default()) {
        Ok(mut s) => {
            if !s.ends_with('\n') {
                s.push('\n');
            }
            s
        }
        Err(e) => format!("error: could not encode output\nhelp: {e}\n"),
    }
}

/// An insertion-ordered TOON object under construction. Wraps a `serde_json`
/// object map; `set` appends a field, `into_toon` encodes the whole document.
#[derive(Default)]
pub struct Doc(Map<String, Value>);

impl Doc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a field. The value is anything convertible into a JSON value —
    /// numbers stay numbers (unquoted in TOON), strings are quoted only when
    /// ambiguous, `Option::None` becomes `null`, and a `Vec<Value>` of uniform
    /// objects renders as a tabular array.
    pub fn set(&mut self, key: &str, value: impl Into<Value>) -> &mut Self {
        self.0.insert(key.to_string(), value.into());
        self
    }

    /// Append a `help` array of complete next-step commands, but only when there
    /// is at least one — an empty help block is omitted entirely.
    pub fn help(&mut self, items: Vec<String>) -> &mut Self {
        if !items.is_empty() {
            self.0.insert("help".to_string(), Value::from(items));
        }
        self
    }

    /// Encode the assembled object as TOON. Takes `&self` (it only reads) so a
    /// handler can either build statement-by-statement on a `let mut d` or
    /// terminate a `Doc::new().set(..).set(..)` chain — both compile.
    pub fn into_toon(&self) -> String {
        render(&Value::Object(self.0.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_scalars_in_insertion_order() {
        let mut d = Doc::new();
        d.set("unread", 206)
            .set("starred", 17)
            .set("db", "~/x/papr.db");
        assert_eq!(d.into_toon(), "unread: 206\nstarred: 17\ndb: ~/x/papr.db\n");
    }

    #[test]
    fn quotes_number_lookalike_strings() {
        // The whole reason for delegating to the reference encoder: a string
        // that looks like a number/boolean/null must be quoted so a TOON reader
        // decodes it back as a string, not as 42 / true / null.
        let mut d = Doc::new();
        d.set("title", "42").set("flag", "true").set("name", "null");
        assert_eq!(
            d.into_toon(),
            "title: \"42\"\nflag: \"true\"\nname: \"null\"\n"
        );
    }

    #[test]
    fn arrays_of_objects_render_tabular() {
        let mut d = Doc::new();
        d.set(
            "articles",
            json!([
                { "id": 1, "title": "A", "flags": "unread" },
                { "id": 2, "title": "B, C", "flags": "read" },
            ]),
        );
        assert_eq!(
            d.into_toon(),
            "articles[2]{id,title,flags}:\n  1,A,unread\n  2,\"B, C\",read\n"
        );
    }

    #[test]
    fn empty_array_states_zero_count() {
        let mut d = Doc::new();
        d.set("feeds", json!([]));
        assert_eq!(d.into_toon(), "feeds[0]:\n");
    }

    #[test]
    fn help_is_omitted_when_empty_and_listed_when_present() {
        let mut empty = Doc::new();
        empty.set("ok", "done").help(vec![]);
        assert_eq!(empty.into_toon(), "ok: done\n");

        let mut with = Doc::new();
        with.set("count", 1)
            .help(vec!["Run `papr read <id>`".to_string()]);
        assert_eq!(
            with.into_toon(),
            "count: 1\nhelp[1]: Run `papr read <id>`\n"
        );
    }

    #[test]
    fn none_renders_as_null() {
        let mut d = Doc::new();
        d.set("author", Value::Null).set("title", "Hi");
        assert_eq!(d.into_toon(), "author: null\ntitle: Hi\n");
    }
}
