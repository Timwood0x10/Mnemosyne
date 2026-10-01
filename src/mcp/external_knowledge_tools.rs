//! MCP tools for external knowledge integration (external-knowledge-plan §E).
//!
//! Registers three tools that let the agent attach external knowledge sources,
//! materialize them into the graph, and compile AI conversations into three-
//! state facts:
//!
//! | Tool                  | Purpose                                                |
//! |-----------------------|--------------------------------------------------------|
//! | `knowledge_attach`    | Register a document (PDF/JSON/TXT/MD) or JSON-DB as a  |
//! |                       | searchable adapter; rebuilds the entity linker.        |
//! | `knowledge_ingest`    | Materialize an attached source into the graph as       |
//! |                       | documents + chapters + evidence.                       |
//! | `agent_fact_compile`  | Compile an AI conversation into user/agent/derived     |
//! |                       | facts with explicit agent-channel opt-in.              |
//!
//! ## Design notes
//!
//! - The registry and entity linker are shared (`Arc`) so `knowledge_attach`
//!   mutates the same registry the retrieval engine reads at query time
//!   (interior mutability via `RwLock`, see `knowledge/external.rs`).
//! - `knowledge_ingest` materialize mode persists docs through the
//!   [`KnowledgeStore`] API so the existing `evidence` / `inspect_entity` tools
//!   can query the materialized content (dev_guide "事实来自编译").
//! - `agent_fact_compile` defaults to `include_agent_facts=false` so the
//!   agent channel is opt-in (plan §C2: "agent 不替用户表态").

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::cognition::FactStore as CognitionFactStore;
use crate::cognition_compiler::CognitionCompiler;
use crate::error::Error;
use crate::fact_store::SqliteFactStore;
use crate::knowledge::adapter::{
    DbAdapter, DocumentAdapter, EntityLink, ExternalHit, ExternalQueryFn, SchemaMapping,
};
use crate::knowledge::format::{self, FormatKind};
use crate::knowledge::store::KnowledgeStore;
use crate::knowledge::{Chapter, Document, Evidence, SQLiteKnowledgeStore};
use crate::mcp::knowledge_tools::SharedEntityLinker;
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler, identity_arg};
use crate::types::Message;

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Extract a required `&str` argument from the JSON args object.
fn req_str(args: &Value, key: &str) -> Result<String, Error> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| Error::InvalidInput(format!("missing required argument `{key}`")))
}

/// Extract an optional `&str` argument from the JSON args object.
fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

/// Build a success [`ToolCallResult`] containing a single JSON text block.
fn json_ok(value: &impl serde::Serialize) -> Result<ToolCallResult, Error> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Internal(format!("serialize result: {e}")))?;
    Ok(ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(text),
            mime_type: Some("application/json".into()),
        }],
        is_error: false,
    })
}

/// Parse the `messages` array from a tool call into [`Message`] values.
fn parse_messages(arr: &[Value]) -> Result<Vec<Message>, Error> {
    let mut out = Vec::with_capacity(arr.len());
    for raw in arr {
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::InvalidInput("each message must be an object".into()))?;
        let role = obj
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `role`".into()))?;
        let content = obj
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("message missing `content`".into()))?;
        let mut msg = Message::new(role, content);
        if let Some(t) = obj.get("turn_id").and_then(Value::as_str) {
            msg.turn_id = Some(t.to_string());
        }
        if let Some(t) = obj.get("tool_call_id").and_then(Value::as_str) {
            msg.tool_call_id = Some(t.to_string());
        }
        out.push(msg);
    }
    Ok(out)
}

/// Parse an array of `{external_name, canonical_name, source}` objects into
/// [`EntityLink`]s. Returns an empty vec when the argument is absent.
fn parse_entity_links(args: &Value) -> Result<Vec<EntityLink>, Error> {
    let Some(arr) = args.get("entity_links").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut links = Vec::with_capacity(arr.len());
    for raw in arr {
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::InvalidInput("each entity_link must be an object".into()))?;
        links.push(EntityLink {
            external_name: obj
                .get("external_name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("entity_link missing `external_name`".into()))?
                .to_string(),
            canonical_name: obj
                .get("canonical_name")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("entity_link missing `canonical_name`".into()))?
                .to_string(),
            source: obj
                .get("source")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidInput("entity_link missing `source`".into()))?
                .to_string(),
        });
    }
    Ok(links)
}

/// Rebuild the shared entity linker from the registry's current links.
fn rebuild_linker(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    linker: &SharedEntityLinker,
) {
    let fresh = registry.build_entity_linker();
    let mut guard = linker
        .write()
        .expect("entity linker lock poisoned (rebuild)");
    *guard = fresh;
}

/// Whether `path` names engine configuration or state rather than source text.
///
/// The allowlist is the whole resource root, which is the installation
/// directory — where `config/`, the SQLite databases and (in a real deployment)
/// credentials live. A client could therefore attach `config/…` or a `.db` file
/// and read it back out through the evidence tools, which is a file-read
/// primitive rather than an attach (audit H1). These names are refused outright;
/// corpus text is unaffected.
fn is_denied_content_path(path: &Path) -> bool {
    // A `config` component anywhere in the path is engine configuration: the
    // shipped tables there describe the runtime, not the corpus.
    if path
        .components()
        .any(|component| component.as_os_str() == "config")
    {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        // A directory, or a name we cannot read: nothing to attach.
        return true;
    };
    let name = name.to_ascii_lowercase();
    if name.starts_with(".env") {
        return true;
    }
    const DENIED_SUFFIXES: &[&str] = &[
        ".db",
        ".db-wal",
        ".db-shm",
        ".db-journal",
        ".sqlite",
        ".sqlite3",
    ];
    DENIED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

/// Reject a file whose bytes are not text.
///
/// `detect_format` falls back to `Text` for an extension it does not know, so a
/// binary file used to be decoded as text and stored as evidence — which turned
/// attach into "read any host file and fetch it back through the evidence tool"
/// (audit H1). A NUL byte is the classic binary marker and costs one pass over
/// the first block. Recognised binary formats (PDF) are exempt: they have a real
/// parser and are expected to contain NUL.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] when the head of the file contains a NUL byte,
/// or [`Error::Io`] when it cannot be read.
fn reject_binary_content(path: &Path, original: &str) -> Result<(), Error> {
    use std::io::Read;

    let mut file = std::fs::File::open(path).map_err(Error::Io)?;
    let mut head = [0_u8; 8192];
    let read = file.read(&mut head).map_err(Error::Io)?;
    if head[..read].contains(&0) {
        return Err(Error::InvalidInput(format!(
            "`{original}` looks binary (NUL byte within the first {read} bytes); \
             only text documents are attachable"
        )));
    }
    Ok(())
}

/// Confirm that a normalized path is inside one of `roots` and names something
/// the attach tool may read, then return its canonical form.
///
/// The lexical checks above are not sufficient on their own:
///
/// - **Symlinks.** `starts_with` compares the *text* of a path, so a link placed
///   inside an allowlisted directory (`root/link -> /etc`) passed the check while
///   the read followed it straight out of the sandbox. `canonicalize` resolves
///   links, so the allowlist is applied to what the path actually *is*; the roots
///   are canonicalized too, because on macOS the temp directory is `/var/…` while
///   the resolved path is `/private/var/…` and comparing the two forms would
///   reject legitimate files.
/// - **Engine state**, see [`is_denied_content_path`] — checked before the read
///   and again afterwards, because a link can point INTO `config/`.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] when the path cannot be resolved, escapes
/// every allowlisted root, or names engine configuration or a database.
fn verify_attachable(path: PathBuf, roots: &[PathBuf], original: &str) -> Result<PathBuf, Error> {
    if is_denied_content_path(&path) {
        return Err(Error::InvalidInput(format!(
            "engine configuration and database files are not attachable; rejected: {original}"
        )));
    }
    let resolved = path
        .canonicalize()
        .map_err(|e| Error::InvalidInput(format!("cannot resolve `{original}`: {e}")))?;
    let inside = roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| resolved.starts_with(root))
    });
    if !inside {
        return Err(Error::InvalidInput(format!(
            "path escapes the allowlisted roots; rejected: {original}"
        )));
    }
    if is_denied_content_path(&resolved) {
        return Err(Error::InvalidInput(format!(
            "engine configuration and database files are not attachable; rejected: {original}"
        )));
    }
    Ok(resolved)
}

/// Resolve a client-supplied knowledge path inside an allowlisted root.
///
/// Policy (mirrors `memory_transfer_tools::resolve_transfer_path` for
/// relative paths, plus two absolute roots for legitimate tooling/tests):
///
/// - Relative paths are joined onto the resource root; `..` escapes are
///   rejected after normalization.
/// - Absolute paths are accepted ONLY under the system temp directory
///   (tests / ephemeral scratch) or under the resource root. `/etc/passwd`
///   and similar paths are rejected.
///
/// # Errors
///
/// Returns [`Error::InvalidInput`] when the path escapes every allowlisted
/// root.
fn resolve_knowledge_path(path: &str) -> Result<PathBuf, Error> {
    let p = PathBuf::from(path);
    let root = crate::config::resolve_resource_path("");
    let temp = std::env::temp_dir();

    if p.is_absolute() {
        // Absolute: only temp or resource-root subtrees are allowed.
        let mut normalized = PathBuf::new();
        for comp in p.components() {
            match comp {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(Error::InvalidInput(format!(
                            "path escapes the allowlisted roots; rejected: {path}"
                        )));
                    }
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        if !(normalized.starts_with(&temp) || normalized.starts_with(&root)) {
            return Err(Error::InvalidInput(format!(
                "absolute path must live under the resource root or the system temp directory; rejected: {path}"
            )));
        }
        return verify_attachable(normalized, &[temp, root], path);
    }

    // Relative: join onto the resource root and reject `..` escapes.
    let joined = root.join(&p);
    let mut normalized = PathBuf::new();
    for comp in joined.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::InvalidInput(format!(
                        "path escapes the resource root; rejected: {path}"
                    )));
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.starts_with(&root) {
        return Err(Error::InvalidInput(format!(
            "path escapes the resource root; rejected: {path}"
        )));
    }
    verify_attachable(normalized, &[root], path)
}

// ── knowledge_attach ────────────────────────────────────────────────────────

/// Handler for the `knowledge_attach` tool.
///
/// Attaches a document (PDF/JSON/TXT/MD) or a JSON-backed DB as a searchable
/// adapter. Document adapters are materialization-only; DB adapters are
/// index-mode (query-forwarded into hybrid search via RRF).
struct KnowledgeAttachHandler {
    registry: Arc<crate::knowledge::ExternalKnowledgeRegistry>,
    linker: SharedEntityLinker,
}

#[async_trait::async_trait]
impl ToolHandler for KnowledgeAttachHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        // Attaching reads files and mutates the shared registry; the whole body
        // is synchronous, so hand it to the blocking pool and keep a large
        // document load off the tokio worker (audit 09-26/H7).
        let registry = Arc::clone(&self.registry);
        let linker = Arc::clone(&self.linker);
        let args = args.clone();
        crate::mcp::blocking::run(move || run_attach_tool(&registry, &linker, &args)).await
    }
}

/// Synchronous body of `knowledge_attach`, executed on the blocking pool.
fn run_attach_tool(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    linker: &SharedEntityLinker,
    args: &Value,
) -> Result<ToolCallResult, Error> {
    let source_type = req_str(args, "source_type")?;
    match source_type.as_str() {
        "document" => attach_document(registry, linker, args),
        "db" => attach_db(registry, linker, args),
        other => Ok(err_result(format!(
            "unknown source_type `{other}`; expected `document` or `db`"
        ))),
    }
}

/// Attach a document file (PDF/JSON/TXT/MD) as a materialization-only
/// adapter. The file is loaded immediately via the format loader; the
/// resulting `ExternalDoc`s are wrapped in a `DocumentAdapter` and
/// registered with the shared registry.
///
/// Runs on the blocking pool (see [`run_attach_tool`]), so the load is inline.
fn attach_document(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    linker: &SharedEntityLinker,
    args: &Value,
) -> Result<ToolCallResult, Error> {
    let path = req_str(args, "path")?;
    let source_name = opt_str(args, "source_name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Default: derive from the file stem so provenance is readable.
            PathBuf::from(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document")
                .to_string()
        });
    let links = parse_entity_links(args)?;

    // Path sandbox: reject absolute paths and `..` escapes (mirror of
    // `memory_transfer_tools::resolve_transfer_path`). Without it a
    // client could `knowledge_attach {path:"/etc/passwd"}` then
    // `knowledge_ingest` and read arbitrary host files through `evidence`.
    let load_path = resolve_knowledge_path(&path)?;
    // The rule is "the loader is about to treat these bytes as prose": a
    // `.txt` file promises to be text and a PDF is expected to be binary, but
    // an extension the loader does not recognise (`notes.bin`, or none at
    // all) falls back to `Text` and would be decoded as prose — which is the
    // case that has to be checked.
    let declared_text = load_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"));
    if format::detect_format(&load_path) == FormatKind::Text && !declared_text {
        reject_binary_content(&load_path, &path)?;
    }

    // Load the file through the multi-format loader (PDF/JSON/TXT/MD). The
    // handler already runs on the blocking pool (see `run_attach_tool`), so
    // the file read — and PDF inflation for large PDFs — happens inline.
    let docs = format::load_document(&load_path)?;
    let doc_count = docs.len();
    let kind = format::detect_format(&PathBuf::from(&path));

    // Build the adapter and attach entity links if any were supplied.
    let adapter = DocumentAdapter::new(source_name.clone(), docs).with_links(links);
    registry.register_document(Arc::new(adapter));
    // Rebuild the linker so inspect_entity sees the new cross-source names.
    rebuild_linker(registry, linker);

    let payload = serde_json::json!({
        "source_name": source_name,
        "source_type": "document",
        "format": format_kind_str(kind),
        "documents_loaded": doc_count,
        "entity_links_registered": registry.collect_entity_links().len(),
        "mode": "materialize-only (documents are NOT query-forwarded; use knowledge_ingest to materialize into the graph)",
    });
    json_ok(&payload)
}

/// Attach a JSON-backed DB as an index-mode (query-forwarded) adapter.
///
/// The `connection` argument is a path to a JSON file containing either a
/// bare array of `{id, text, score?}` objects or a `{"documents": [...]}`
/// wrapper. On `search`, the adapter does case-insensitive substring
/// matching against `text` and returns hits sorted by score. This avoids a
/// heavy SQL dependency while demonstrating the DB index-mode path
/// (rules.md: no network, fast builds).
fn attach_db(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    linker: &SharedEntityLinker,
    args: &Value,
) -> Result<ToolCallResult, Error> {
    let connection = req_str(args, "connection")?;
    let source_name = opt_str(args, "source_name")
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            PathBuf::from(&connection)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("db")
                .to_string()
        });
    let links = parse_entity_links(args)?;

    // Path sandbox: same rule as attach_document (no absolute / no `..`).
    let db_path = resolve_knowledge_path(&connection)?;
    let db_path_str = db_path.to_string_lossy().into_owned();

    // Load the JSON DB file once and snapshot it into the query closure.
    // Already on the blocking pool, so the read happens inline.
    let rows = load_json_db(&db_path_str)?;
    let row_count = rows.len();
    let query_fn: ExternalQueryFn =
        Box::new(move |query: &str, limit: usize| search_json_db(&rows, query, limit));

    let adapter =
        DbAdapter::new(source_name.clone(), SchemaMapping::default(), query_fn).with_links(links);
    registry.register_signal(adapter);
    rebuild_linker(registry, linker);

    let payload = serde_json::json!({
        "source_name": source_name,
        "source_type": "db",
        "connection": connection,
        "rows_indexed": row_count,
        "entity_links_registered": registry.collect_entity_links().len(),
        "mode": "index-mode (query-forwarded into hybrid search via RRF; not materialized by default)",
    });
    json_ok(&payload)
}

/// One row of a JSON-backed DB, snapshot into the query closure.
#[derive(Debug)]
struct JsonDbRow {
    id: String,
    text: String,
    score: f64,
}

/// Load a JSON DB file into in-memory rows. Accepts a bare array or a
/// `{"documents": [...]}` wrapper. Each entry must have `id` and `text`;
/// `score` defaults to 0.5.
fn load_json_db(path: &str) -> Result<Vec<JsonDbRow>, Error> {
    let bytes =
        std::fs::read(path).map_err(|e| Error::Internal(format!("read db file `{path}`: {e}")))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::InvalidInput(format!("db file `{path}` is not valid JSON: {e}")))?;
    let arr = match &value {
        Value::Array(a) => a,
        Value::Object(o) => o
            .get("documents")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::InvalidInput(format!(
                    "db file `{path}` must be an array or {{\"documents\": [...]}}"
                ))
            })?,
        _ => {
            return Err(Error::InvalidInput(format!(
                "db file `{path}` must be a JSON array or object"
            )));
        }
    };
    let mut rows = Vec::with_capacity(arr.len());
    for raw in arr {
        let obj = raw
            .as_object()
            .ok_or_else(|| Error::InvalidInput("each db row must be an object".into()))?;
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("db row missing `id`".into()))?
            .to_string();
        let text = obj
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::InvalidInput("db row missing `text`".into()))?
            .to_string();
        let score = obj
            .get("score")
            .and_then(Value::as_f64)
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        rows.push(JsonDbRow { id, text, score });
    }
    Ok(rows)
}

/// Case-insensitive substring search over the JSON DB rows. Returns up to
/// `limit` hits sorted by (score desc, id asc) for deterministic ordering.
fn search_json_db(rows: &[JsonDbRow], query: &str, limit: usize) -> Vec<ExternalHit> {
    if limit == 0 {
        return Vec::new();
    }
    let needle = query.to_ascii_lowercase();
    let mut hits: Vec<ExternalHit> = rows
        .iter()
        .filter(|row| {
            // Empty query matches everything (used by materialize-all).
            needle.is_empty() || row.text.to_ascii_lowercase().contains(&needle)
        })
        .map(|row| ExternalHit {
            id: row.id.clone(),
            text: row.text.clone(),
            score: row.score,
            source: "db".into(),
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    hits.truncate(limit);
    hits
}

/// Map a [`FormatKind`] to a human-readable string for the attach response.
fn format_kind_str(kind: FormatKind) -> &'static str {
    match kind {
        FormatKind::Text => "text",
        FormatKind::Markdown => "markdown",
        FormatKind::Json => "json",
        FormatKind::Pdf => "pdf",
    }
}

// ── knowledge_ingest ────────────────────────────────────────────────────────

/// Handler for the `knowledge_ingest` tool.
///
/// Materializes an attached source into the graph: each `ExternalDoc` becomes
/// a `Document` + `Chapter` + `Evidence` row in the knowledge store so the
/// existing `evidence` / `inspect_entity` tools can query it.
struct KnowledgeIngestHandler {
    registry: Arc<crate::knowledge::ExternalKnowledgeRegistry>,
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for KnowledgeIngestHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        let source_name = req_str(args, "source_name")?;
        let mode = opt_str(args, "mode").unwrap_or("materialize");

        match mode {
            "index" => {
                // Reporting the registry state is synchronous; run it on the
                // blocking pool (audit 09-26/H7).
                let registry = Arc::clone(&self.registry);
                crate::mcp::blocking::run(move || run_ingest_index(&registry, &source_name)).await
            }
            // Materialize persists through the async `KnowledgeStore`, so those
            // awaits stay on the async shell (see `materialize`).
            "materialize" => self.materialize(&source_name).await,
            other => Ok(err_result(format!(
                "unknown mode `{other}`; expected `index` or `materialize`"
            ))),
        }
    }
}

/// Synchronous body of the `index` mode of `knowledge_ingest`: report the
/// current registry state without materializing anything.
fn run_ingest_index(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    source_name: &str,
) -> Result<ToolCallResult, Error> {
    let payload = serde_json::json!({
        "source_name": source_name,
        "mode": "index",
        "status": "index-mode sources are query-forwarded; no materialization performed",
        "signal_providers": registry.signal_provider_count(),
    });
    json_ok(&payload)
}

/// Collect the documents of `source_name` (or every source when `"all"`) from
/// the registry, on the blocking pool.
fn materialize_docs(
    registry: &crate::knowledge::ExternalKnowledgeRegistry,
    source_name: &str,
) -> Result<Vec<crate::knowledge::adapter::ExternalDoc>, Error> {
    if source_name == "all" {
        registry.materialize_all()
    } else {
        registry.materialize_source(source_name)
    }
}

impl KnowledgeIngestHandler {
    /// Materialize docs from `source_name` (or all sources when `"all"`) into
    /// the knowledge store as documents + chapters + evidence.
    ///
    /// Collecting the documents from the registry is synchronous work, so it is
    /// handed to the blocking pool; the per-document store writes all go
    /// through the async [`KnowledgeStore`] and therefore stay on this shell
    /// (audit 09-26/H7).
    ///
    /// [`KnowledgeStore`]: crate::knowledge::store::KnowledgeStore
    async fn materialize(&self, source_name: &str) -> Result<ToolCallResult, Error> {
        let registry = Arc::clone(&self.registry);
        let source = source_name.to_owned();
        let docs = crate::mcp::blocking::run(move || materialize_docs(&registry, &source)).await?;
        if docs.is_empty() {
            return Ok(err_result(format!(
                "source `{source_name}` has no documents to materialize"
            )));
        }

        let mut doc_count = 0usize;
        let mut chapter_count = 0usize;
        let mut evidence_count = 0usize;
        for ext in &docs {
            // Skip if the same (title, source) document already exists
            // (idempotent re-ingest — dev_guide "不双写" spirit).
            let existing = self.store.find_document(&ext.title, &ext.source).await?;
            let doc_id = if let Some(doc) = existing {
                doc.id
            } else {
                let doc = Document {
                    id: 0,
                    title: ext.title.clone(),
                    author: ext.author.clone(),
                    doc_type: Some(ext.doc_type.clone()),
                    source: ext.source.clone(),
                    created_at: chrono::Utc::now().timestamp(),
                };
                let did = self.store.create_document(&doc).await?;
                doc_count += 1;
                did
            };

            // Create a chapter for the doc body. Chapter number comes from
            // `ext.chapter` when present, else 1. A chapter with the same
            // (doc_id, chapter_no) is reused, not re-inserted — repeated
            // ingest previously duplicated every chapter unconditionally
            // (half-idempotent: documents were deduped but chapters and
            // evidence piled up on every call).
            let chapter_no = ext.chapter.unwrap_or(1);
            let chapter_id = match self.store.get_chapter_by_no(doc_id, chapter_no).await? {
                Some(ch) => ch.id,
                None => {
                    let chapter = Chapter {
                        id: 0,
                        doc_id,
                        chapter_no,
                        title: Some(ext.title.clone()),
                        content: ext.text.clone(),
                        start_offset: Some(0),
                        end_offset: Some(ext.text.len() as i64),
                    };
                    let cid = self.store.create_chapter(&chapter).await?;
                    chapter_count += 1;
                    cid
                }
            };

            // Persist the body as evidence so `evidence` search can find it.
            // An evidence row with the same content already under this doc is
            // skipped (idempotent re-ingest must not duplicate evidence).
            let already_evidenced = self
                .store
                .list_evidence_by_document(doc_id)
                .await?
                .iter()
                .any(|e| e.content == ext.text);
            if !already_evidenced {
                let evidence = Evidence {
                    id: 0,
                    doc_id,
                    chapter_id,
                    start_offset: Some(0),
                    end_offset: Some(ext.text.len() as i64),
                    content: ext.text.clone(),
                    created_at: chrono::Utc::now().timestamp(),
                };
                self.store.create_evidence(&evidence).await?;
                evidence_count += 1;
            }
        }

        let payload = serde_json::json!({
            "source_name": source_name,
            "mode": "materialize",
            "documents_created": doc_count,
            "chapters_created": chapter_count,
            "evidence_created": evidence_count,
            "total_materialized": docs.len(),
        });
        json_ok(&payload)
    }
}

// ── agent_fact_compile ──────────────────────────────────────────────────────

/// Handler for the `agent_fact_compile` tool.
///
/// Compiles an AI conversation into three-state facts (user/agent/derived)
/// via [`CognitionCompiler::compile_conversation_facts`]. User facts are
/// always persisted; agent + derived facts require `include_agent_facts=true`
/// (plan §C2: agent channel is opt-in).
struct AgentFactCompileHandler {
    fact_store: Arc<SqliteFactStore>,
}

#[async_trait::async_trait]
impl ToolHandler for AgentFactCompileHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult, Error> {
        // The compiler and the fact-store writes are synchronous `rusqlite`;
        // run them on the blocking pool (audit 09-26/H7).
        let fact_store = Arc::clone(&self.fact_store);
        let args = args.clone();
        crate::mcp::blocking::run(move || run_agent_fact_compile_tool(&fact_store, &args)).await
    }
}

/// Synchronous body of `agent_fact_compile`, executed on the blocking pool.
fn run_agent_fact_compile_tool(
    fact_store: &SqliteFactStore,
    args: &Value,
) -> Result<ToolCallResult, Error> {
    let messages_raw = args
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidInput("missing `messages` array".into()))?;
    let messages = parse_messages(messages_raw)?;
    let tenant_id = identity_arg(args, "tenant_id");
    let user_id = identity_arg(args, "user_id");
    let agent_id = opt_str(args, "agent_id").unwrap_or("");
    let include_agent_facts = args
        .get("include_agent_facts")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Resolve the User and Agent entity ids. resolve_user/resolve_agent
    // namespace by external_key so the two channels never collide
    // (plan §C2: "agent 不替用户表态").
    let user_entity_id = fact_store.resolve_user(tenant_id, user_id)?;
    let agent_entity_id = fact_store.resolve_agent(tenant_id, agent_id)?;

    // Logical time: use the current epoch second so facts are chronologically
    // orderable alongside compiler-emitted facts.
    let logical_time = chrono::Utc::now().timestamp();

    let compiler = CognitionCompiler::new();
    let facts = compiler.compile_conversation_facts(
        &messages,
        user_entity_id,
        agent_entity_id,
        logical_time,
    );

    // Always persist user facts (first-hand user cognition).
    let user_count = if facts.user_facts.is_empty() {
        0
    } else {
        fact_store.insert_batch(&facts.user_facts)?
    };

    // Agent + derived facts are opt-in: only persist when the caller
    // explicitly enables the agent channel.
    let (agent_count, derived_count) = if include_agent_facts {
        let a = if facts.agent_facts.is_empty() {
            0
        } else {
            fact_store.insert_batch(&facts.agent_facts)?
        };
        let d = if facts.derived_facts.is_empty() {
            0
        } else {
            fact_store.insert_batch(&facts.derived_facts)?
        };
        (a, d)
    } else {
        (0, 0)
    };

    let payload = serde_json::json!({
        "tenant_id": tenant_id,
        "user_id": user_id,
        "agent_id": agent_id,
        "user_entity_id": user_entity_id,
        "agent_entity_id": agent_entity_id,
        "include_agent_facts": include_agent_facts,
        "user_facts_persisted": user_count,
        "agent_facts_persisted": agent_count,
        "derived_facts_persisted": derived_count,
        "total_extracted": facts.total(),
        "note": if include_agent_facts {
            "agent channel enabled; agent events + derived restatements persisted with attribution"
        } else {
            "agent channel disabled (default); only user facts persisted. set include_agent_facts=true to enable agent + derived channels"
        },
    });
    json_ok(&payload)
}

/// Build an error [`ToolCallResult`] whose content is a human-readable message.
fn err_result(msg: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(msg.into()),
            mime_type: None,
        }],
        is_error: true,
    }
}

// ── Public entry point ──────────────────────────────────────────────────────

/// Register the external-knowledge MCP tools on `builder`
/// (external-knowledge-plan §E).
///
/// `registry` is the shared registry attached to the retrieval engine;
/// `linker` is the shared cross-source entity linker consumed by
/// `inspect_entity`; `store` persists materialized documents; `fact_store`
/// persists compiled conversation facts.
pub async fn register_external_knowledge_tools(
    builder: ServerBuilder,
    registry: Arc<crate::knowledge::ExternalKnowledgeRegistry>,
    linker: SharedEntityLinker,
    store: Arc<SQLiteKnowledgeStore>,
    fact_store: Arc<SqliteFactStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "knowledge_attach".into(),
                description: "Attach an external knowledge source as a searchable adapter. Documents (PDF/JSON/TXT/MD) are materialization-only; JSON-backed DBs are index-mode (query-forwarded into hybrid search). Rebuilds the entity linker so inspect_entity resolves cross-source names.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source_type": {"type": "string", "enum": ["document", "db"], "description": "document = PDF/JSON/TXT/MD file; db = JSON-backed DB file"},
                        "path": {"type": "string", "description": "File path (required for source_type=document)"},
                        "connection": {"type": "string", "description": "JSON DB file path (required for source_type=db). File must be an array of {id, text, score?} or {\"documents\": [...]}"},
                        "source_name": {"type": "string", "description": "Optional friendly name; defaults to the file stem"},
                        "entity_links": {
                            "type": "array",
                            "description": "Optional cross-source entity links [{external_name, canonical_name, source}]",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "external_name": {"type": "string"},
                                    "canonical_name": {"type": "string"},
                                    "source": {"type": "string"}
                                },
                                "required": ["external_name", "canonical_name", "source"]
                            }
                        }
                    },
                    "required": ["source_type"]
                }),
            },
            Arc::new(KnowledgeAttachHandler {
                registry: registry.clone(),
                linker: linker.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "knowledge_ingest".into(),
                description: "Materialize an attached external source into the knowledge graph as documents + chapters + evidence. Use source_name='all' to materialize every attached source.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "source_name": {"type": "string", "description": "Attached source name, or 'all' for every source"},
                        "mode": {"type": "string", "enum": ["index", "materialize"], "default": "materialize", "description": "index = report status only; materialize = persist docs into the graph"}
                    },
                    "required": ["source_name"]
                }),
            },
            Arc::new(KnowledgeIngestHandler {
                registry: registry.clone(),
                store: store.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "agent_fact_compile".into(),
                description: "Compile an AI conversation into three-state facts (user/agent/derived). User facts are always persisted; agent + derived facts require include_agent_facts=true (agent channel is opt-in so agent never speaks for the user).".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "messages": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "role": {"type": "string", "enum": ["user", "assistant", "system"]},
                                    "content": {"type": "string"},
                                    "tool_call_id": {"type": "string"},
                                    "turn_id": {"type": "string"}
                                },
                                "required": ["role", "content"]
                            }
                        },
                        "tenant_id": {"type": "string", "default": "default"},
                        "user_id": {"type": "string"},
                        "agent_id": {"type": "string"},
                        "include_agent_facts": {"type": "boolean", "default": false, "description": "Enable the agent + derived fact channels (opt-in)"}
                    },
                    "required": ["messages"]
                }),
            },
            Arc::new(AgentFactCompileHandler { fact_store }),
        )
        .await
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

/// Unit tests for the external-knowledge MCP tools.
///
/// Extracted into a sibling file via `#[path]` so the module source stays
/// under the 1000-line limit (plan/rules/rules.md §1).
#[cfg(test)]
#[path = "external_knowledge_tools_tests.rs"]
mod tests;
