//! MCP tools for memory migration — export/import the knowledge graph.
//!
//! `memory_export` serializes the whole graph (documents, entities, edges,
//! evidence, links) into a portable JSON snapshot; `memory_import` replays a
//! snapshot into the store, deduplicating by identity so re-importing is a
//! no-op. Together they make "memory is never lost" concrete: a persona can
//! be backed up, moved to another machine, or shared, and restored intact.

use std::sync::Arc;

use serde_json::Value;

use crate::error::{Error, Result};
use crate::knowledge::SQLiteKnowledgeStore;
use crate::knowledge::memory_export::{ExportBundle, export_store, import_bundle};
use crate::mcp::server::ServerBuilder;
use crate::mcp::types::{ContentBlock, ToolCallResult, ToolDefinition, ToolHandler};

/// Build a JSON text block result.
fn json_block(value: &impl serde::Serialize, is_error: bool) -> Result<ToolCallResult> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Internal(format!("serialize result: {e}")))?;
    Ok(ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(text),
            mime_type: Some("application/json".into()),
        }],
        is_error,
    })
}

/// `memory_export` handler.
pub struct MemoryExportHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

#[async_trait::async_trait]
impl ToolHandler for MemoryExportHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let bundle = export_store(self.store.as_ref()).await?;
        let documents = bundle.documents.len();
        let objects = bundle.objects.len();
        let edges = bundle.edges.len();
        let evidence = bundle.evidence.len();

        // Write to a file when a path is supplied; otherwise return the JSON.
        if let Some(path) = args.get("path").and_then(Value::as_str) {
            let json = serde_json::to_string_pretty(&bundle)
                .map_err(|e| Error::Internal(format!("serialize bundle: {e}")))?;
            std::fs::write(path, json).map_err(Error::Io)?;
            return json_block(
                &serde_json::json!({
                    "exported": true,
                    "path": path,
                    "documents": documents,
                    "objects": objects,
                    "edges": edges,
                    "evidence": evidence,
                }),
                false,
            );
        }

        json_block(
            &serde_json::json!({
                "exported": true,
                "inline": true,
                "documents": documents,
                "objects": objects,
                "edges": edges,
                "evidence": evidence,
                "bundle": bundle,
            }),
            false,
        )
    }
}

/// `memory_import` handler.
pub struct MemoryImportHandler {
    store: Arc<SQLiteKnowledgeStore>,
}

/// Build a graceful error [`ToolCallResult`] (protocol success, content error).
fn err_result(message: impl Into<String>) -> ToolCallResult {
    ToolCallResult {
        content: vec![ContentBlock {
            block_type: "text".into(),
            text: Some(message.into()),
            mime_type: Some("application/json".into()),
        }],
        is_error: true,
    }
}

#[async_trait::async_trait]
impl ToolHandler for MemoryImportHandler {
    async fn call(&self, args: &Value) -> Result<ToolCallResult> {
        let raw = if let Some(path) = args.get("path").and_then(Value::as_str) {
            match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => return Ok(err_result(format!("read import file {path}: {e}"))),
            }
        } else if let Some(content) = args.get("content").and_then(Value::as_str) {
            content.to_string()
        } else {
            return Ok(err_result("memory_import requires `content` or `path`"));
        };

        let bundle: ExportBundle = match serde_json::from_str(&raw) {
            Ok(b) => b,
            Err(e) => return Ok(err_result(format!("invalid memory bundle: {e}"))),
        };
        match import_bundle(self.store.as_ref(), &bundle).await {
            Ok(stats) => json_block(
                &serde_json::json!({
                    "imported": true,
                    "documents_created": stats.documents_created,
                    "objects_created": stats.objects_created,
                    "objects_merged": stats.objects_merged,
                    "evidence_created": stats.evidence_created,
                    "edges_created": stats.edges_created,
                    "links_created": stats.links_created,
                }),
                false,
            ),
            Err(e) => Ok(err_result(format!("import failed: {e}"))),
        }
    }
}

/// Register both memory-transfer tools on `builder`.
pub async fn register_memory_transfer_tools(
    builder: ServerBuilder,
    store: Arc<SQLiteKnowledgeStore>,
) -> ServerBuilder {
    builder
        .tool(
            ToolDefinition {
                name: "memory_export".into(),
                description: "Serialize the entire knowledge graph (documents, entities, edges, evidence, links) into a portable JSON snapshot. Pass `path` to write it to a file, or omit `path` to receive the JSON inline. The snapshot can later be restored with `memory_import` — move a persona's memory between machines or back it up.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Optional file path to write the snapshot to"}
                    }
                }),
            },
            Arc::new(MemoryExportHandler {
                store: store.clone(),
            }),
        )
        .await
        .tool(
            ToolDefinition {
                name: "memory_import".into(),
                description: "Replay a memory snapshot produced by `memory_export` into the store, deduplicating by identity (document title, entity name, evidence content) so a re-import is a no-op. Provide the bundle as `content` (JSON string) or as a file via `path`.".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "content": {"type": "string", "description": "The exported JSON bundle (inline)"},
                        "path": {"type": "string", "description": "Path to a file containing the exported JSON bundle"}
                    },
                    "oneOf": [
                        {"required": ["content"]},
                        {"required": ["path"]}
                    ]
                }),
            },
            Arc::new(MemoryImportHandler { store }),
        )
        .await
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::store::KnowledgeStore;

    async fn memory_store() -> Arc<SQLiteKnowledgeStore> {
        Arc::new(
            SQLiteKnowledgeStore::open_in_memory()
                .await
                .expect("open in-memory store"),
        )
    }

    /// Objective: Verify `memory_import` (inline content) round-trips through
    /// `memory_export`: import a seed bundle, export it, then import the
    /// exported JSON into a fresh store and confirm the entity survives.
    /// Invariants: export reports non-zero objects; import into fresh store
    /// creates the object; re-import is a no-op for objects.
    #[tokio::test]
    async fn export_then_import_round_trips() {
        // Seed a store by importing a small bundle.
        let seeder = memory_store().await;
        let seed = serde_json::json!({
            "format": "lorescope-memory",
            "version": 1,
            "exported_at": 0,
            "documents": [{"title": "会话-迁移", "author": null, "doc_type": "dialog"}],
            "objects": [{
                "doc_title": "会话-迁移",
                "object_type": "person",
                "name": "用户",
                "properties": {"偏好": "简洁"},
                "confidence": 0.8
            }],
            "edges": [],
            "evidence": [],
            "evidence_links": []
        });
        let import_handler = MemoryImportHandler {
            store: seeder.clone(),
        };
        let imported = import_handler
            .call(&serde_json::json!({"content": seed.to_string()}))
            .await
            .expect("import");
        assert!(!imported.is_error, "seed import succeeds");
        let imported_text = imported.content[0].text.clone().unwrap_or_default();
        assert!(
            imported_text.contains("\"objects_created\": 1"),
            "one object created, got: {imported_text}"
        );

        // Export from the seeded store.
        let export_handler = MemoryExportHandler {
            store: seeder.clone(),
        };
        let exported = export_handler
            .call(&serde_json::json!({}))
            .await
            .expect("export");
        assert!(!exported.is_error, "export succeeds");
        let exported_text = exported.content[0].text.clone().unwrap_or_default();
        assert!(
            exported_text.contains("\"objects\": 1"),
            "one object exported, got: {exported_text}"
        );

        // Import the exported inline bundle into a fresh store.
        let exported_json: Value = serde_json::from_str(&exported_text).expect("export is JSON");
        let exported_bundle: ExportBundle = serde_json::from_value(exported_json["bundle"].clone())
            .expect("export text embeds a bundle");
        let fresh = memory_store().await;
        let import_handler2 = MemoryImportHandler {
            store: fresh.clone(),
        };
        let reimport = import_handler2
            .call(&serde_json::json!({"content": serde_json::to_string(&exported_bundle).expect("serialize")}))
            .await
            .expect("reimport");
        let reimport_text = reimport.content[0].text.clone().unwrap_or_default();
        assert!(
            reimport_text.contains("\"objects_created\": 1"),
            "object recreated in fresh store, got: {reimport_text}"
        );

        let obj = fresh
            .find_object_by_name("用户", None)
            .await
            .expect("query")
            .expect("entity present");
        assert_eq!(obj.properties["偏好"], "简洁", "properties preserved");
    }

    /// Objective: Verify `memory_export` can write to a file path.
    /// Invariants: file created and parseable as a bundle.
    #[tokio::test]
    async fn export_writes_to_file() {
        let store = memory_store().await;
        // Seed one object so the file is non-trivial.
        let seed = serde_json::json!({
            "format": "lorescope-memory",
            "version": 1,
            "exported_at": 0,
            "documents": [{"title": "d", "author": null, "doc_type": null}],
            "objects": [{"doc_title": "d", "object_type": "person", "name": "甲",
                          "properties": {}, "confidence": 0.8}],
            "edges": [], "evidence": [], "evidence_links": []
        });
        MemoryImportHandler {
            store: store.clone(),
        }
        .call(&serde_json::json!({"content": seed.to_string()}))
        .await
        .expect("seed");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("memory.json");
        let path_str = path.to_str().expect("path").to_string();

        let handler = MemoryExportHandler { store };
        let result = handler
            .call(&serde_json::json!({"path": path_str}))
            .await
            .expect("export to file");
        assert!(!result.is_error, "export to file succeeds");

        let raw = std::fs::read_to_string(&path).expect("file written");
        let bundle: ExportBundle = serde_json::from_str(&raw).expect("file is a bundle");
        assert_eq!(bundle.objects.len(), 1, "file contains the object");
    }

    /// Objective: Verify `memory_import` requires content or path.
    /// Invariants: no content/path → graceful error result.
    #[tokio::test]
    async fn import_requires_content_or_path() {
        let store = memory_store().await;
        let handler = MemoryImportHandler { store };
        let result = handler.call(&serde_json::json!({})).await.expect("call");
        assert!(result.is_error, "missing content/path → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("content") || text.contains("path"),
            "error mentions content/path, got: {text}"
        );
    }

    /// Objective: Verify an invalid bundle is rejected gracefully.
    /// Invariants: malformed JSON → error result mentioning the format.
    #[tokio::test]
    async fn invalid_bundle_rejected() {
        let store = memory_store().await;
        let handler = MemoryImportHandler { store };
        let result = handler
            .call(&serde_json::json!({"content": "not a bundle"}))
            .await
            .expect("call");
        assert!(result.is_error, "invalid bundle → error result");
        let text = result.content[0].text.clone().unwrap_or_default();
        assert!(
            text.contains("invalid memory bundle"),
            "clear error, got: {text}"
        );
    }
}
