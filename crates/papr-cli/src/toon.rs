//! A small TOON (Token-Oriented Object Notation) emitter, shaped for the exact
//! AXI output forms this CLI produces: scalar key/value blocks, tabular arrays
//! (`name[N]{f1,f2}:` + comma-delimited rows), inline help lists, and long-text
//! blocks. We build TOON only at the output boundary — every handler assembles
//! one `Out` and prints it.
//!
//! TOON is ~40% cheaper than the equivalent JSON in tokens while staying
//! readable; see <https://toonformat.dev>.

use std::fmt::Write as _;

/// Quote/escape a single scalar value if it would otherwise be ambiguous in
/// TOON (contains a delimiter, a quote, a newline, or edge whitespace). A plain
/// word or a phrase with interior spaces is emitted bare — spaces alone never
/// need quoting.
pub fn scalar(value: &str) -> String {
    let needs_quote = value.is_empty()
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
        || value.contains([',', '"', '\n', '\r', '\t']);
    if !needs_quote {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// An optional value: `None` renders as a bare `-` so an absent field is
/// unambiguous (and distinct from the empty string `""`).
pub fn opt(value: Option<&str>) -> String {
    match value {
        Some(v) => scalar(v),
        None => "-".to_string(),
    }
}

/// Incrementally-built TOON document.
#[derive(Default)]
pub struct Out {
    buf: String,
}

impl Out {
    pub fn new() -> Self {
        Self::default()
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.buf.push_str("  ");
        }
    }

    /// A raw line at the given indent depth (no key processing).
    pub fn line(&mut self, depth: usize, text: &str) -> &mut Self {
        self.indent(depth);
        self.buf.push_str(text);
        self.buf.push('\n');
        self
    }

    /// `key: value`, with the value scalar-quoted as needed.
    pub fn kv(&mut self, depth: usize, key: &str, value: &str) -> &mut Self {
        self.indent(depth);
        let _ = write!(self.buf, "{key}: {}", scalar(value));
        self.buf.push('\n');
        self
    }

    /// `key:` header introducing a nested block.
    pub fn header(&mut self, depth: usize, key: &str) -> &mut Self {
        self.indent(depth);
        self.buf.push_str(key);
        self.buf.push_str(":\n");
        self
    }

    /// A tabular array:
    /// ```text
    /// name[N]{f1,f2}:
    ///   a,b
    ///   c,d
    /// ```
    /// Rows are already-formatted cells; pass them through [`scalar`] at the
    /// call site or use [`Out::row`].
    pub fn table(&mut self, depth: usize, name: &str, fields: &[&str], rows: &[Vec<String>]) -> &mut Self {
        self.indent(depth);
        let _ = write!(self.buf, "{name}[{}]{{{}}}:", rows.len(), fields.join(","));
        self.buf.push('\n');
        for row in rows {
            self.indent(depth + 1);
            self.buf.push_str(&row.join(","));
            self.buf.push('\n');
        }
        self
    }

    /// A help list of complete next-step commands:
    /// ```text
    /// help[2]:
    ///   Run `x` to ...
    ///   Run `y` to ...
    /// ```
    pub fn help(&mut self, items: &[String]) -> &mut Self {
        if items.is_empty() {
            return self;
        }
        let _ = write!(self.buf, "help[{}]:\n", items.len());
        for item in items {
            self.indent(1);
            self.buf.push_str(item);
            self.buf.push('\n');
        }
        self
    }

    /// A long-text block: a `key:` header followed by the (already truncated)
    /// text indented one level, each source line preserved. Trailing notes such
    /// as the truncation marker are passed in `text` already.
    pub fn block(&mut self, depth: usize, key: &str, text: &str) -> &mut Self {
        self.header(depth, key);
        for line in text.split('\n') {
            self.line(depth + 1, line);
        }
        self
    }

    pub fn into_string(self) -> String {
        self.buf
    }
}
