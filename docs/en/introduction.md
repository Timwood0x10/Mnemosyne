# Cognitive Memory MCP Server — Introduction

## What is Memory Distillation?

**Memory Distillation** is a Model Context Protocol (MCP) server that gives AI coding agents **persistent long-term memory**. Unlike context compression techniques that only keep information within a single session, Memory Distillation extracts, classifies, and persists knowledge from conversations so that agents can recall it in future sessions — across days, weeks, or months.

### The Problem: Agents That Don't Learn

Current AI coding agents suffer from a fundamental limitation:

| Approach | What it does | After session ends |
|---|---|---|
| Built-in context compression | Keeps current conversation within token window | **Forgets everything** |
| RTK (Reduced Token Ken) | Compresses shell output | **Forgets everything** |
| **Memory Distillation** | Extracts + persists knowledge | **Still remembers** |

The result: users ask the same question twice and pay tokens twice. Agents have no mechanism to carry lessons, preferences, or decisions from one session to the next.

### The Solution: Distillation as Learning

Memory Distillation treats each conversation as a learning opportunity. At the end of a session — or on demand — it runs an 8-stage pipeline that:

1. **Extracts** problem-solution pairs from the conversation
2. **Classifies** each pair as knowledge, skill, preference, experience, interaction, or profile
3. **Scores** importance so trivial chatter is naturally deprioritized
4. **Filters** out noise, secrets, and security-sensitive content
5. **Compresses** into a compact `问题：解决方案` (Problem: Solution) format
6. **Embeds** into vectors (or falls back to keyword-only mode for zero API cost)
7. **Resolves** conflicts against existing memories using cosine similarity
8. **Persists** with per-tenant capacity control and LRU eviction

When a new session begins, relevant memories are injected into the agent's context — it knows what it learned before, without wasting tokens to re-discover it.

## Key Design Principles

### 1. Zero-Cost Keyword Mode

The server runs with **zero API dependencies**. Set `--embedding-provider none` and `--retrieval-mode keyword` — no API keys, no embedding models, no vector database. SQLite FTS5 provides full-text search at no ongoing cost. Vector search is an optional upgrade, not a requirement.

### 2. Deterministic Pipeline

Every stage in the distillation pipeline is deterministic — no LLM calls, no random sampling, no non-deterministic heuristics. This makes the system **testable, predictable, and auditable**. The same input always produces the same output.

### 3. Tenant Isolation

Built for multi-tenant scenarios from the ground up. Every memory is scoped to a tenant ID, retrieval is tenant-filtered, and capacity limits (default 5000 per type per tenant) are enforced independently for each tenant.

### 4. MCP Native

Implements the standard Model Context Protocol over stdio transport. Compatible with any MCP client — Claude Desktop, VS Code extensions, custom toolchains — with zero integration friction.

## What This Is NOT

- **Not** a context compression replacement — it is **long-term memory** that complements compression
- **Not** an LLM proxy — it handles no LLM inference
- **Not** a general-purpose database — it is purpose-built for agent memory
- **Not** cloud-only — runs entirely locally

## Use Cases

| Scenario | Benefit |
|---|---|
| **Software development** | Agent remembers project conventions, past bug fixes, architecture decisions across sessions |
| **DevOps / SRE** | Agent recalls incident response procedures, runbook decisions, environment quirks |
| **Data analysis** | Agent retains analysis patterns, library preferences, domain knowledge |
| **Personal assistant** | Agent remembers user preferences, common tasks, workflow idioms |
| **Research** | Agent builds a persistent knowledge base from exploration sessions |

## Getting Help

- **GitHub Issues**: https://github.com/TimWood/memory_distill/issues
- **Quick Start**: See [getting-started.md](getting-started.md)
- **Architecture**: See [architecture.md](architecture.md)
