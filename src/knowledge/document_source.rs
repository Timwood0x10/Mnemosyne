//! Unified document source abstraction.
//!
//! Every external knowledge source — a file (txt/pdf/json/md, already routed
//! through `format::load_document`) or a conversation (dialog messages) —
//! emits the SAME intermediate representation: [`ExternalDoc`]. Downstream
//! consumers (the cognition compiler, the knowledge store, value extraction)
//! then treat every source uniformly; a dialog is just another document
//! whose `doc_type` is `"dialog"`.
//!
//! This is the "unify all data sources" step of the generalization plan:
//! one trait, one output shape, pluggable sources.

use crate::error::Result;
use crate::knowledge::adapter::ExternalDoc;
use crate::types::Message;

/// A source of documents, uniform across files and conversations.
pub trait DocumentSource: Send + Sync {
    /// Load all documents this source yields, in the unified [`ExternalDoc`]
    /// shape.
    ///
    /// # Errors
    ///
    /// Source-specific (I/O, parse, invalid input). Never panics.
    fn load(&self) -> Result<Vec<ExternalDoc>>;
}

/// A file-backed source: dispatches on extension via `format::load_document`
/// (txt / markdown / json / pdf), preserving the existing loaders.
pub struct FileSource {
    /// Absolute or workspace-relative path to the file.
    path: std::path::PathBuf,
}

impl FileSource {
    /// Create a file source for `path`.
    #[must_use]
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl DocumentSource for FileSource {
    fn load(&self) -> Result<Vec<ExternalDoc>> {
        crate::knowledge::format::load_document(&self.path)
    }
}

/// A conversation source: `Message[]` → one document per conversation, with
/// speaker roles preserved in the text (`user:` / `assistant:` prefixes) so
/// downstream extraction can still attribute statements.
pub struct DialogSource {
    /// Conversation title (used as the document title).
    title: String,
    /// Origin identifier for provenance.
    source: String,
    /// The conversation messages, in order.
    messages: Vec<Message>,
}

impl DialogSource {
    /// Create a dialog source.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        source: impl Into<String>,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            title: title.into(),
            source: source.into(),
            messages,
        }
    }
}

impl DocumentSource for DialogSource {
    fn load(&self) -> Result<Vec<ExternalDoc>> {
        if self.messages.is_empty() {
            return Ok(Vec::new());
        }
        // Flatten messages into a single text body, tagging each speaker.
        // Role prefixes are stripped of surrounding whitespace but kept
        // verbatim otherwise — provenance over prettiness.
        let mut body =
            String::with_capacity(self.messages.iter().map(|m| m.content.len() + 8).sum());
        for m in &self.messages {
            body.push_str(&m.role);
            body.push_str(": ");
            body.push_str(m.content.trim());
            body.push('\n');
        }
        Ok(vec![ExternalDoc {
            title: self.title.clone(),
            text: body,
            chapter: None,
            source: self.source.clone(),
            doc_type: "dialog".into(),
            author: None,
        }])
    }
}

/// A raw-text source: arbitrary caller-provided text with no backing file or
/// conversation. This is the most general entry point — any string of prose,
/// notes, chat transcript pasted directly, or structured records flattened to
/// text can be compiled without materializing a file on disk.
pub struct RawTextSource {
    /// Document title (used as the top-level entity anchor).
    title: String,
    /// Origin identifier for provenance.
    source: String,
    /// Arbitrary text body.
    text: String,
    /// Free-form document type tag (e.g. `"text"`, `"notes"`, `"memory"`).
    doc_type: String,
}

impl RawTextSource {
    /// Create a raw-text source.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        source: impl Into<String>,
        text: impl Into<String>,
        doc_type: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            source: source.into(),
            text: text.into(),
            doc_type: doc_type.into(),
        }
    }
}

impl DocumentSource for RawTextSource {
    fn load(&self) -> Result<Vec<ExternalDoc>> {
        let body = self.text.trim();
        if body.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![ExternalDoc {
            title: self.title.clone(),
            text: body.to_string(),
            chapter: None,
            source: self.source.clone(),
            doc_type: self.doc_type.clone(),
            author: None,
        }])
    }
}

/// Convenience: load a single source through the trait (object-safety
/// wrapper used by tools that hold a boxed source).
///
/// # Errors
///
/// Delegates to the source's own error.
pub fn load_source(source: &dyn DocumentSource) -> Result<Vec<ExternalDoc>> {
    source.load()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    /// Objective: Verify a dialog source flattens messages into one document
    /// with speaker-role prefixes.
    /// Invariants: one ExternalDoc, doc_type "dialog", text contains both
    /// "user:" and "assistant:" lines; title/source propagated.
    #[test]
    fn dialog_source_flattens_messages() {
        let source = DialogSource::new(
            "会话A",
            "export.json",
            vec![
                Message::new("user", "我想要落地这个方案"),
                Message::new("assistant", "好的，我来规划"),
            ],
        );
        let docs = load_source(&source).expect("load");
        assert_eq!(docs.len(), 1, "one conversation → one document");
        assert_eq!(docs[0].doc_type, "dialog");
        assert_eq!(docs[0].title, "会话A");
        assert_eq!(docs[0].source, "export.json");
        assert!(
            docs[0].text.contains("user: 我想要落地这个方案"),
            "user line with role prefix expected, got: {:?}",
            docs[0].text
        );
        assert!(
            docs[0].text.contains("assistant: 好的，我来规划"),
            "assistant line with role prefix expected"
        );
    }

    /// Objective: Verify an empty dialog yields NO documents (not a
    /// malformed empty-string doc).
    /// Invariants: empty messages → empty vec; no panic.
    #[test]
    fn empty_dialog_yields_no_documents() {
        let source = DialogSource::new("空会话", "export.json", Vec::new());
        let docs = load_source(&source).expect("load");
        assert!(docs.is_empty(), "no messages → no documents");
    }

    /// Objective: Verify message whitespace is trimmed but role/order kept.
    /// Invariants: leading/trailing spaces are stripped; order preserved.
    #[test]
    fn dialog_trimming_preserves_order() {
        let source = DialogSource::new(
            "t",
            "s",
            vec![
                Message::new("user", "  第一条  "),
                Message::new("assistant", "  第二条\n换行"),
            ],
        );
        let docs = load_source(&source).expect("load");
        assert!(docs[0].text.starts_with("user: 第一条\n"), "leading trim");
        assert!(
            docs[0].text.contains("assistant: 第二条\n换行"),
            "content after role kept verbatim (internal newline preserved)"
        );
    }

    /// Objective: Verify FileSource routes through the existing format
    /// loader for a real txt file.
    /// Invariants: a temp txt yields one ExternalDoc with doc_type "text".
    #[test]
    fn file_source_loads_txt() {
        let path = std::env::temp_dir().join(format!(
            "lorescope_doc_source_test_{}_{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::write(&path, "测试文档内容\n").expect("write temp");
        let source = FileSource::new(&path);
        let docs = source.load().expect("load");
        assert_eq!(docs.len(), 1);
        assert!(docs[0].text.contains("测试文档内容"));
        let _ = std::fs::remove_file(&path);
    }

    /// Objective: Verify FileSource surfaces I/O errors as typed errors
    /// (never a panic on missing files).
    /// Invariants: nonexistent path → Err.
    #[test]
    fn file_source_missing_path_errors() {
        let source = FileSource::new("/nonexistent/lorescope_does_not_exist.txt");
        let result = source.load();
        assert!(
            matches!(result, Err(Error::Io(_))),
            "missing file → Io error"
        );
    }

    /// Objective: Verify RawTextSource wraps arbitrary prose into a single
    /// ExternalDoc without touching disk, keeping title/source/doc_type.
    /// Invariants: 1 doc; text verbatim; tags preserved.
    #[test]
    fn raw_text_source_roundtrip() {
        let source = RawTextSource::new(
            "我的记忆",
            "paste",
            "  我偏爱简洁的架构设计，反对过度抽象。  ",
            "notes",
        );
        let docs = load_source(&source).expect("load");
        assert_eq!(docs.len(), 1, "one text blob → one document");
        assert_eq!(docs[0].doc_type, "notes");
        assert_eq!(docs[0].title, "我的记忆");
        assert_eq!(docs[0].source, "paste");
        assert_eq!(
            docs[0].text, "我偏爱简洁的架构设计，反对过度抽象。",
            "trimmed"
        );
    }

    /// Objective: Verify RawTextSource yields NO documents for blank input.
    /// Invariants: whitespace-only text → empty vec; no panic.
    #[test]
    fn raw_text_blank_yields_nothing() {
        let source = RawTextSource::new("t", "s", "   \n\t ", "text");
        let docs = load_source(&source).expect("load");
        assert!(docs.is_empty(), "blank text → no documents");
    }
}
