//! Document Compiler — Phase 1.
//!
//! Loads raw text from files, raw strings, or message streams, detects
//! document metadata (title, author, type), and produces a [`Document`]
//! ready for chunking.
//!
//! ## Supported input formats
//!
//! | Source         | Entry point        | Metadata detection                          |
//! |----------------|--------------------|--------------------------------------------|
//! | `.txt` file    | [`from_file`]      | filename → title, extension → doc_type     |
//! | Raw string     | [`from_text`]      | caller provides title/type                 |
//! | Messages       | [`from_messages`]  | "conversation" type, first user msg → title |

use std::path::Path;

/// Input document metadata and full raw text.
#[derive(Debug, Clone)]
pub struct Document {
    pub title: String,
    pub author: Option<String>,
    pub source: String,
    pub doc_type: String,
    pub text: String,
}

impl Document {
    /// Load a document from a text file (UTF-8).
    ///
    /// The title is inferred from the file stem; `doc_type` is inferred from
    /// a hardcoded map (or falls back to `"text"`).
    ///
    /// # Errors
    ///
    /// Returns `Io` if the file cannot be read.
    pub fn from_file(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        let title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_owned();
        let doc_type = detect_type(path);
        Ok(Document {
            title,
            author: None,
            source: path.to_string_lossy().into_owned(),
            doc_type,
            text,
        })
    }

    /// Create a document from a raw string.
    ///
    /// The caller provides `title` and `doc_type` directly.
    pub fn from_text(
        title: impl Into<String>,
        doc_type: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Document {
            title: title.into(),
            author: None,
            source: "raw".into(),
            doc_type: doc_type.into(),
            text: text.into(),
        }
    }

    /// Create a conversation document from message slices.
    ///
    /// Each message is concatenated as `"role: content\n"`.
    /// The title is derived from the first user message (truncated to 64 chars).
    pub fn from_messages(messages: &[crate::types::Message]) -> Self {
        let title = messages
            .iter()
            .find(|m| m.is_user())
            .map(|m| {
                let t: String = m.content.chars().take(64).collect();
                t
            })
            .unwrap_or_else(|| "conversation".into());

        let text: String = messages
            .iter()
            .map(|m| format!("{}: {}\n", m.role, m.content))
            .collect();

        Document {
            title,
            author: None,
            source: "conversation".into(),
            doc_type: "conversation".into(),
            text,
        }
    }

    /// Total byte length of the raw text.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// True when the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Infer `doc_type` from the file extension.
fn detect_type(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("txt") => "text",
        Some("md") | Some("markdown") => "markdown",
        Some("json") => "json",
        Some("csv") => "csv",
        Some("pdf") => "pdf",
        _ => "text",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    /// Objective: Verify that `from_file` sets the title from the file stem.
    /// Invariants: The document title equals the file name without extension.
    #[test]
    fn document_title_from_file_stem() {
        let d =
            Document::from_file("corpus/三国演义.txt").expect("三国演义.txt should be readable");
        assert_eq!(d.title, "三国演义", "title should be the file stem");
        assert_eq!(d.doc_type, "text", "txt file → doc_type=text");
        assert!(!d.text.is_empty(), "text should be non-empty");
    }

    /// Objective: Verify that `from_text` preserves caller-supplied metadata.
    /// Invariants: Title and doc_type match exactly what was passed.
    #[test]
    fn document_from_text_preserves_metadata() {
        let d = Document::from_text("测试文档", "test", "这是正文内容。");
        assert_eq!(d.title, "测试文档");
        assert_eq!(d.doc_type, "test");
        assert_eq!(d.text, "这是正文内容。");
        assert_eq!(d.source, "raw");
    }

    /// Objective: Verify that `from_messages` builds a document with concatenated text.
    /// Invariants: Each message appears as `role: content\n` in the text; title
    /// comes from the first user message (truncated to 64 chars).
    #[test]
    fn document_from_messages_concatenates() {
        let msgs = vec![
            Message::new("user", "今天天气怎么样？"),
            Message::new("assistant", "天气很好。"),
        ];
        let d = Document::from_messages(&msgs);
        assert_eq!(d.doc_type, "conversation");
        assert!(d.text.contains("user: 今天天气怎么样？"));
        assert!(d.text.contains("assistant: 天气很好。"));
        assert_eq!(d.title, "今天天气怎么样？");
    }

    /// Objective: Verify behaviour with an empty message list.
    /// Invariants: Title falls back to "conversation"; text is empty.
    #[test]
    fn document_from_messages_empty() {
        let d = Document::from_messages(&[]);
        assert_eq!(d.title, "conversation");
        assert!(d.text.is_empty());
    }

    /// Objective: Verify that `from_file` on a non-existent file returns an error.
    /// Invariants: The result is `Err` (not a panic).
    #[test]
    fn document_from_missing_file_errors() {
        let result = Document::from_file("/nonexistent/path.txt");
        assert!(result.is_err(), "missing file should produce an error");
    }

    /// Objective: Verify that `detect_type` handles various extensions correctly.
    /// Invariants: Known extensions map to their expected types; unknown fall back to "text".
    #[test]
    fn detect_type_handles_extensions() {
        let cases = vec![
            ("doc.txt", "text"),
            ("readme.md", "markdown"),
            ("data.json", "json"),
            ("report.pdf", "pdf"),
            ("unknown.xyz", "text"),
            ("NOEXT", "text"),
        ];
        for (name, expected) in cases {
            assert_eq!(
                detect_type(Path::new(name)),
                expected,
                "extension detection for {name}"
            );
        }
    }
}
