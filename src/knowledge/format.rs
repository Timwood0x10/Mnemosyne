//! Multi-format document loader — turns external files into [`ExternalDoc`]s.
//!
//! Supported formats (detected by extension):
//!
//! | Extension | Loader          | Output `doc_type` |
//! |-----------|-----------------|-------------------|
//! | `.txt`    | [`TextLoader`]  | `text`            |
//! | `.md`     | [`MarkdownLoader`] | `markdown`     |
//! | `.json`   | [`JsonLoader`]  | `json`            |
//! | `.pdf`    | [`PdfLoader`]   | `pdf`             |
//!
//! Every loader returns `Vec<ExternalDoc>`: JSON may carry many documents in
//! one file; text/markdown/pdf produce one document each (PDF text is extracted
//! as a single body). All output is ready for the compiler pipeline — no raw
//! text is injected into the graph without compilation (dev_guide "事实来自编译").

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::knowledge::adapter::ExternalDoc;
use crate::knowledge::pdf;

// ───────────────────────────────────────────────────────────────────────────
// FormatKind + detect
// ───────────────────────────────────────────────────────────────────────────

/// Detected document format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatKind {
    Text,
    Markdown,
    Json,
    Pdf,
}

/// Infer the format from a file extension. Unknown extensions fall back to
/// plain text so a missing extension never blocks ingestion.
#[must_use]
pub fn detect_format(path: &Path) -> FormatKind {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt") => FormatKind::Text,
        Some("md") | Some("markdown") => FormatKind::Markdown,
        Some("json") => FormatKind::Json,
        Some("pdf") => FormatKind::Pdf,
        _ => FormatKind::Text,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// FormatLoader trait + dispatchers
// ───────────────────────────────────────────────────────────────────────────

/// A loader that converts bytes (+ a display name) into [`ExternalDoc`]s.
pub trait FormatLoader: Send + Sync {
    fn load_from_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>>;
}

/// Load all documents from a file path, dispatching on extension.
///
/// # Errors
///
/// Returns [`Error::Io`] if the file cannot be read, or a loader-specific
/// error (e.g. [`Error::InvalidInput`] for malformed JSON / PDF).
pub fn load_document(path: &Path) -> Result<Vec<ExternalDoc>> {
    let bytes = std::fs::read(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_owned();
    let source = path.to_string_lossy().into_owned();
    load_kind_with_source(detect_format(path), &name, &source, &bytes)
}

/// Load all documents from raw bytes, dispatching on an explicit format.
#[must_use]
pub fn loader_for(kind: FormatKind) -> Box<dyn FormatLoader> {
    match kind {
        FormatKind::Text => Box::new(TextLoader),
        FormatKind::Markdown => Box::new(MarkdownLoader),
        FormatKind::Json => Box::new(JsonLoader),
        FormatKind::Pdf => Box::new(PdfLoader),
    }
}

fn load_kind_with_source(
    kind: FormatKind,
    name: &str,
    source: &str,
    bytes: &[u8],
) -> Result<Vec<ExternalDoc>> {
    match kind {
        FormatKind::Text => TextLoader.load_with_source(name, source, bytes),
        FormatKind::Markdown => MarkdownLoader.load_with_source(name, source, bytes),
        FormatKind::Json => JsonLoader.load_with_source(name, source, bytes),
        FormatKind::Pdf => PdfLoader.load_with_source(name, source, bytes),
    }
}

/// Shared helper: run a loader impl while injecting the caller's `source`.
trait SourceLoader {
    fn load_with_source(&self, name: &str, source: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>>;
}

// ───────────────────────────────────────────────────────────────────────────
// Text / Markdown loaders
// ───────────────────────────────────────────────────────────────────────────

/// Plain-text loader: one document per file.
pub struct TextLoader;
/// Markdown loader: one document per file (`doc_type = "markdown"`).
pub struct MarkdownLoader;

impl SourceLoader for TextLoader {
    fn load_with_source(&self, name: &str, source: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        let text = decode_utf8_lossy(bytes)?;
        Ok(vec![ExternalDoc {
            title: name.to_owned(),
            text,
            chapter: None,
            source: source.to_owned(),
            doc_type: "text".into(),
            author: None,
        }])
    }
}

impl SourceLoader for MarkdownLoader {
    fn load_with_source(&self, name: &str, source: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        let text = decode_utf8_lossy(bytes)?;
        Ok(vec![ExternalDoc {
            title: name.to_owned(),
            text,
            chapter: None,
            source: source.to_owned(),
            doc_type: "markdown".into(),
            author: None,
        }])
    }
}

impl FormatLoader for TextLoader {
    fn load_from_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        self.load_with_source(name, name, bytes)
    }
}

impl FormatLoader for MarkdownLoader {
    fn load_from_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        self.load_with_source(name, name, bytes)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// JSON loader
// ───────────────────────────────────────────────────────────────────────────

/// JSON loader. Accepts three shapes:
/// - A bare array `[{ ... }, ...]`
/// - An object `{ "documents": [ ... ] }`
/// - A single object `{ ... }`
///
/// Each entry may carry `title`, `text` (required), `chapter`, `source`,
/// `author`; missing optional fields default to `None`/the file name.
pub struct JsonLoader;

/// Deserialization shape for one JSON document entry.
#[derive(Debug, Deserialize)]
struct JsonDoc {
    title: Option<String>,
    text: Option<String>,
    chapter: Option<i32>,
    #[serde(default)]
    source: Option<String>,
    author: Option<String>,
}

impl SourceLoader for JsonLoader {
    fn load_with_source(&self, name: &str, source: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::InvalidInput(format!("json parse: {e}")))?;

        // Accept three shapes: `{documents: [...]}`, a bare array, or a single
        // object. The wrapper case is checked first by cloning its `documents`
        // value so the outer `value` can still be moved in the else branch.
        let entries: Vec<JsonDoc> = if let Some(docs) = value.get("documents").cloned() {
            serde_json::from_value::<Vec<JsonDoc>>(docs)
                .map_err(|e| Error::InvalidInput(format!("json documents: {e}")))?
        } else {
            match value {
                serde_json::Value::Array(_) => serde_json::from_value::<Vec<JsonDoc>>(value)
                    .map_err(|e| Error::InvalidInput(format!("json entries: {e}")))?,
                serde_json::Value::Object(_) => {
                    let doc: JsonDoc = serde_json::from_value(value)
                        .map_err(|e| Error::InvalidInput(format!("json entry: {e}")))?;
                    vec![doc]
                }
                other => {
                    return Err(Error::InvalidInput(format!(
                        "json document must be array, object, or {{documents:[]}}, got {}",
                        json_kind(&other)
                    )));
                }
            }
        };

        let mut out = Vec::with_capacity(entries.len());
        for (i, entry) in entries.into_iter().enumerate() {
            let text = entry.text.ok_or_else(|| {
                Error::InvalidInput(format!("json entry {i} missing required `text` field"))
            })?;
            out.push(ExternalDoc {
                title: entry.title.unwrap_or_else(|| format!("{name}#{i}")),
                text,
                chapter: entry.chapter,
                source: entry.source.unwrap_or_else(|| source.to_owned()),
                doc_type: "json".into(),
                author: entry.author,
            });
        }
        Ok(out)
    }
}

impl FormatLoader for JsonLoader {
    fn load_from_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        self.load_with_source(name, name, bytes)
    }
}

/// Human-readable JSON node kind for error messages.
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

// ───────────────────────────────────────────────────────────────────────────
// PDF loader
// ───────────────────────────────────────────────────────────────────────────

/// PDF loader: extracts plain text via [`pdf::extract_text`] into one document.
pub struct PdfLoader;

impl SourceLoader for PdfLoader {
    fn load_with_source(&self, name: &str, source: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        let text = pdf::extract_text(bytes)?;
        Ok(vec![ExternalDoc {
            title: name.to_owned(),
            text,
            chapter: None,
            source: source.to_owned(),
            doc_type: "pdf".into(),
            author: None,
        }])
    }
}

impl FormatLoader for PdfLoader {
    fn load_from_bytes(&self, name: &str, bytes: &[u8]) -> Result<Vec<ExternalDoc>> {
        self.load_with_source(name, name, bytes)
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

/// Decode bytes as UTF-8 (lossy) — text/markdown files are assumed UTF-8.
fn decode_utf8_lossy(bytes: &[u8]) -> Result<String> {
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Objective: Verify detect_format maps every supported extension.
    /// Invariants: txt→Text, md→Markdown, json→Json, pdf→Pdf, unknown→Text.
    #[test]
    fn detect_format_maps_extensions() {
        let cases = [
            ("notes.txt", FormatKind::Text),
            ("readme.md", FormatKind::Markdown),
            ("readme.markdown", FormatKind::Markdown),
            ("data.json", FormatKind::Json),
            ("report.pdf", FormatKind::Pdf),
            ("noext", FormatKind::Text),
            ("weird.xyz", FormatKind::Text),
        ];
        for (name, expected) in cases {
            assert_eq!(
                detect_format(&PathBuf::from(name)),
                expected,
                "detect_format({name})"
            );
        }
    }

    /// Objective: Verify TextLoader produces one doc carrying the file stem as
    /// title and the raw text as body.
    /// Invariants: One ExternalDoc; doc_type="text"; text equals the input.
    #[test]
    fn text_loader_produces_one_doc() {
        let docs = TextLoader
            .load_with_source("notes", "/tmp/notes.txt", b"hello world")
            .expect("load");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "notes");
        assert_eq!(docs[0].text, "hello world");
        assert_eq!(docs[0].doc_type, "text");
        assert_eq!(docs[0].source, "/tmp/notes.txt");
        assert!(docs[0].chapter.is_none());
    }

    /// Objective: Verify MarkdownLoader tags doc_type as markdown.
    /// Invariants: doc_type="markdown"; body preserved verbatim.
    #[test]
    fn markdown_loader_tags_type() {
        let docs = MarkdownLoader
            .load_with_source("readme", "/tmp/readme.md", b"# Title\nbody")
            .expect("load");
        assert_eq!(docs[0].doc_type, "markdown");
        assert!(docs[0].text.contains("# Title"));
    }

    /// Objective: Verify JsonLoader accepts a bare array of entries.
    /// Invariants: Two entries → two ExternalDocs; required `text` preserved;
    /// missing optional fields default sensibly.
    #[test]
    fn json_loader_accepts_bare_array() {
        let json = r#"[
            {"title": "A", "text": "alpha", "chapter": 1},
            {"text": "beta"}
        ]"#;
        let docs = JsonLoader
            .load_with_source("data", "/tmp/data.json", json.as_bytes())
            .expect("load");
        assert_eq!(docs.len(), 2, "two array entries → two docs");
        assert_eq!(docs[0].title, "A");
        assert_eq!(docs[0].chapter, Some(1));
        assert_eq!(
            docs[1].title, "data#1",
            "missing title defaults to name#index"
        );
        assert_eq!(docs[1].text, "beta");
        assert_eq!(
            docs[1].source, "/tmp/data.json",
            "missing source defaults to file path"
        );
    }

    /// Objective: Verify JsonLoader accepts `{documents: [...]}` wrapper.
    /// Invariants: Entries under `documents` are extracted; others ignored.
    #[test]
    fn json_loader_accepts_documents_wrapper() {
        let json = r#"{"documents": [{"title": "X", "text": "x body"}]}"#;
        let docs = JsonLoader
            .load_with_source("data", "data", json.as_bytes())
            .expect("load");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "X");
        assert_eq!(docs[0].text, "x body");
    }

    /// Objective: Verify JsonLoader accepts a single object as one document.
    /// Invariants: One ExternalDoc from the single object.
    #[test]
    fn json_loader_accepts_single_object() {
        let json = r#"{"title": "Solo", "text": "solo body", "author": "A"}"#;
        let docs = JsonLoader
            .load_with_source("data", "data", json.as_bytes())
            .expect("load");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].author.as_deref(), Some("A"));
    }

    /// Objective: Verify malformed JSON returns a typed error, not a panic.
    /// Invariants: Invalid JSON → Err(InvalidInput).
    #[test]
    fn json_loader_rejects_malformed() {
        let err = JsonLoader
            .load_with_source("data", "data", b"not json {")
            .unwrap_err();
        assert!(err.to_string().contains("json parse"), "got: {err}");
    }

    /// Objective: Verify a JSON entry missing the required `text` field errors.
    /// Invariants: Entry without `text` → Err mentioning the field.
    #[test]
    fn json_loader_requires_text_field() {
        let json = r#"[{"title": "no body"}]"#;
        let err = JsonLoader
            .load_with_source("data", "data", json.as_bytes())
            .unwrap_err();
        assert!(
            err.to_string().contains("missing required `text`"),
            "got: {err}"
        );
    }

    /// Objective: Verify the high-level load_document dispatcher reads a real
    /// file and dispatches by extension.
    /// Invariants: A .txt file round-trips through load_document into one doc.
    #[test]
    fn load_document_dispatches_txt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("chapter.txt");
        std::fs::write(&path, "the quick brown fox").expect("write");
        let docs = load_document(&path).expect("load");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "chapter");
        assert_eq!(docs[0].doc_type, "text");
        assert_eq!(docs[0].text, "the quick brown fox");
    }

    /// Objective: Verify load_document dispatches a .json file through the JSON
    /// loader.
    /// Invariants: A .json array file produces one doc per entry.
    #[test]
    fn load_document_dispatches_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("data.json");
        std::fs::write(&path, r#"[{"title":"a","text":"alpha"}]"#).expect("write");
        let docs = load_document(&path).expect("load");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].doc_type, "json");
        assert_eq!(docs[0].text, "alpha");
    }

    /// Objective: Verify load_document rejects a missing file with Io error.
    /// Invariants: Non-existent path → Err (no panic).
    #[test]
    fn load_document_missing_file_errors() {
        let err = load_document(&PathBuf::from("/nonexistent/nope.txt")).unwrap_err();
        assert!(err.to_string().contains("I/O") || err.to_string().contains("No such"));
    }
}
