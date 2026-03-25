# Vault — Specification

Vault is a persistent, file-based knowledge store for agent systems. It accumulates project knowledge (research, discoveries, design decisions, findings) and exposes it through structured operations. It is designed as a standalone library consumed by orchestrators such as epic.

Vault takes a configuration struct as a constructor argument (analogous to reel's two-config pattern). Access is serialized by the orchestrator; vault does not handle concurrent access internally.

### Sibling Projects

Vault is part of a family of repositories under the same GitHub organization. These are typically cloned as sibling directories (e.g., `../epic`, `../reel` relative to vault's root).

| Project | Role | Repository |
|---|---|---|
| **epic** | Orchestrator that consumes vault as its knowledge store | [github.com/bitmonk8/epic](https://github.com/bitmonk8/epic) |
| **reel** | Agent session layer; the librarian is implemented as a reel agent | [github.com/bitmonk8/reel](https://github.com/bitmonk8/reel) |
| **vault** | This project | [github.com/bitmonk8/vault](https://github.com/bitmonk8/vault) |

---

## Configuration

Following reel's pattern, vault uses an environment struct for construction and per-operation config where needed.

```rust
pub struct VaultEnvironment {
    pub storage_root: PathBuf,          // e.g., ".epic/docs/"
    pub model_registry: flick::ModelRegistry,
    pub provider_registry: flick::ProviderRegistry,
    pub models: VaultModels,
}

pub struct VaultModels {
    pub bootstrap: String,              // model ID for Bootstrap operation
    pub query: String,                  // model ID for Query operation
    pub record: String,                 // model ID for Record operation
    pub reorganize: String,             // model ID for Reorganize operation
}
```

`model_registry` and `provider_registry` are required because the librarian is a reel agent that makes LLM calls internally. `models` allows the orchestrator to choose which model handles each operation — e.g., Haiku for lightweight operations like Query and Record, a larger model for Bootstrap or Reorganize if needed.

---

## Storage

Centralized knowledge at a configurable root directory (e.g., `.epic/docs/`). All consumers see all accumulated knowledge, organized by topic. File-based (markdown). Small document counts make this sufficient.

### Core Principle

Documents describe **current reality**. They are not logs, journals, or histories. When information becomes obsolete — a decision is reversed, a constraint is resolved, an approach is abandoned — the old information is **removed** from the document. The document should read as if it were written today. All historical changes are captured in CHANGELOG.md, which is managed programmatically by vault (not by the librarian).

### Document Format

**Naming:** `UPPERCASE_DESCRIPTIVE` names: `FINDINGS.md`, `API_DESIGN.md`, `MIGRATION_PLAN.md`. No numbering prefixes. No lowercase. No sequencing.

**Structure:** Every document uses a standard header:

```markdown
# Document Title
<!-- scope: one-line description of what this document covers -->
<!-- related: OTHER_DOC.md, ANOTHER_DOC.md -->
```

Followed by typed sections appropriate to the document's purpose:

| Section type | Content |
|---|---|
| `## Decisions` | Resolved choices with rationale |
| `## Constraints` | Hard limits, invariants, non-negotiables |
| `## Open Questions` | Unresolved issues requiring future work |
| `## Approach` | Current implementation strategy |
| `## Findings` | Discovered facts, error patterns, observations |
| `## Interfaces` | API surfaces, contracts, protocols |

Not every document needs all section types. Sections may have subsections (`###`) for grouping related items.

**Cross-references:** Use explicit path references: `See DESIGN.md > Authentication` or `See [Authentication](DESIGN.md#authentication)`. Do not duplicate substantial content across documents — reference the authoritative location.

### Document Lifecycle

**Size trigger:** If a document exceeds approximately 200 lines, review whether its largest topic should be split into its own document.

**Coherence trigger:** If information has no natural home in any existing document — if adding it would require a section unrelated to the document's stated scope — create a new document.

Both triggers are considered together. A 50-line document with an off-topic section warrants a new document. A 300-line document where all sections are cohesive may be fine.

When creating a new document: choose an `UPPERCASE_DESCRIPTIVE` name, add the standard header, add appropriate section types, and update PROJECT.md to index the new document.

## Core Documents

| Document | Purpose |
|---|---|
| PROJECT.md | Project overview + document index |
| REQUIREMENTS.md | Captured from interactive session |
| CHANGELOG.md | Append-only mutation log (managed programmatically, not by librarian) |
| FINDINGS.md | Accumulated discoveries |
| DESIGN.md | Design decisions |
| Topic-specific | Created as needed by librarian |

The set of core documents is fixed at bootstrap time. Topic-specific documents are created dynamically by the librarian as the knowledge base grows.

---

## Operations

### 1. Bootstrap

Convert an initial set of requirements into the core document set. This runs once, before any interactive session.

```rust
pub async fn bootstrap(&self, requirements: &str) -> Result<(), VaultError>;
```

Input: raw requirements text. Output: populated core documents on disk. Returns error if the storage root already contains core documents (bootstrap is not idempotent).

### 2. Query

Search documents for relevant knowledge.

```rust
pub struct DocumentRef(pub String); // "FILENAME > Section" format

pub enum Coverage {
    /// Question fully answered from existing documents.
    Full,
    /// Partial answer available; some aspects not covered.
    Partial,
    /// No relevant information found in vault.
    None,
}

pub struct QueryResult {
    pub coverage: Coverage,
    pub answer: String,             // Natural-language answer, suitable for end users
    pub extracts: Vec<Extract>,     // Source material backing the answer
}

pub struct Extract {
    pub content: String,
    pub source: DocumentRef,
}

pub async fn query(&self, question: &str) -> Result<QueryResult, VaultError>;
```

`coverage` is the librarian's structured assessment of how well the vault's existing knowledge answers the question. The Research Service uses this to decide whether further research is needed without parsing the natural-language answer. `answer` is a consolidated response suitable for printing to end users — it may be partial if coverage is `Partial`. `extracts` provide source material for validation and deeper reading.

The orchestrator's Research Service layers on top of this: it calls `query`, checks `coverage` to decide whether gaps exist, fills gaps via external tools if needed, calls `record` to store new findings, then returns its own `ResearchResult` to the caller. Vault is unaware of the Research Service — it only provides storage and retrieval.

### 3. Record

Write findings into vault. The librarian decides file placement.

```rust
pub async fn record(&self, content: &str) -> Result<Vec<DocumentRef>, VaultError>;
```

Input: raw content (findings, decisions, discoveries). Output: references to documents that were created or modified. Callers submit raw content; vault handles organization via the librarian.

**Relevance filter:** The librarian does not record everything. It filters for information that a future task would need to: avoid repeating failed work, make consistent decisions, understand constraints or blockers, or follow established patterns. Raw error logs, intermediate build output, routine progress updates, and information only relevant to the current task's execution are summarized or discarded.

**No restructuring during Record.** The Record operation only adds or updates content relevant to the new information. It does not reorder sections, split documents, or restructure for cosmetic reasons. Restructuring is reserved for the Reorganize operation.

### 4. Reorganize

Trigger a thorough restructuring pass over the entire vault. The librarian reviews all documents for merging opportunities, structural improvements, and deduplication. Unlike the lightweight librarian pass on each Record, this is a full sweep.

```rust
pub async fn reorganize(&self) -> Result<ReorganizeReport, VaultError>;

pub struct ReorganizeReport {
    pub merged: Vec<DocumentRef>,
    pub restructured: Vec<DocumentRef>,
    pub deleted: Vec<DocumentRef>,
}
```

**Trigger conditions:** The orchestrator calls Reorganize at natural checkpoints — e.g., after completing a top-level task, before starting a new phase, or when document count exceeds a threshold. Vault does not self-trigger; the orchestrator decides when.

Note: Reorganize has no counterpart in epic's current DocumentStore design. It exists because the lightweight per-Record librarian pass is insufficient to catch cross-document redundancy that accumulates over time.

---

## Librarian

A reel agent that manages document organization. The model for each operation is configured via `VaultModels`. Invoked internally by vault on Query, Record, and Reorganize. Not exposed to external callers.

**Responsibilities:**
- **Placement**: decides which file and section receives new content.
- **Merging**: combines related content across documents when overlap grows.
- **Restructuring**: reorganizes sections as topics evolve.
- **Deduplication**: prevents repeated information.
- **Growth control**: prevents unbounded document expansion.

**Tool access:** The librarian has read/write access to files within the storage root only. It reads existing documents to decide placement and writes to create, modify, or merge documents. It has no access to tools outside the storage root (no codebase exploration, no web search, no shell commands). The constraint is **scope**: it operates exclusively within the vault storage directory.

### System Prompt Composition

The librarian's system prompt is composed per-operation from shared building blocks and operation-specific instructions. This avoids the duplication problem of maintaining separate guide files with overlapping content (as the Python epic predecessor did with META.md / GUIDE_READ.md / GUIDE_WRITE.md / GUIDE_MAINTAIN.md).

**Shared blocks** (included in every operation's prompt):

| Block | Content |
|---|---|
| Core principle | "Documents describe current reality." Superseding rules. |
| Document format | UPPERCASE_DESCRIPTIVE naming, standard header, typed sections. |
| Cross-references | Path reference format, no content duplication. |
| Scope restriction | Operate only within the storage root. No external tools. |
| Document inventory | Current list of documents with their scope comments. |

**Operation-specific blocks:**

| Operation | Additional blocks |
|---|---|
| Query | Extraction process: start with PROJECT.md for orientation, read relevant documents, synthesize answer with coverage assessment. No writes. |
| Record | Relevance filter, placement rules (read PROJECT.md for index, identify target document and section). Superseding rules (replace, don't append). No restructuring. |
| Reorganize | Full restructuring operations: split, merge, remove, consolidate, reorder, tighten prose. Document lifecycle triggers (size ~200 lines, coherence). Update PROJECT.md after structural changes. |
| Bootstrap | Core document templates (PROJECT.md, REQUIREMENTS.md, CHANGELOG.md). Initialization rules: derive structure from requirements, don't invent structure they don't support. |

Vault assembles the final prompt by concatenating the relevant blocks. The shared blocks are defined once as embedded constants (e.g., `include_str!`). The document inventory block is generated at call time from the current state of the storage root.

This scoping prevents the librarian from restructuring documents during a Record call, or from modifying documents during a Query call — not by convention, but because the instructions for those capabilities are absent from the prompt.

---

## Errors

```rust
pub enum VaultError {
    /// Storage root does not exist or is inaccessible.
    StorageUnavailable(PathBuf),
    /// Bootstrap called on an already-initialized vault.
    AlreadyInitialized,
    /// File I/O failure (read, write, delete).
    Io(std::io::Error),
    /// Librarian agent call failed (model error, timeout).
    LibrarianFailed(String),
}
```

Design choice: vault does not define a "not found" error for Query. A `QueryResult` with `Coverage::None` is the normal response when no documents match — not an error condition.

---

## Integration Contract

Vault is a library consumed by orchestrators (e.g., epic). It provides Bootstrap, Query, and Record as its public API. Higher-level services (e.g., epic's Research Service) layer on top by calling these operations combined with external tools.

Vault participates in a dual-channel context propagation model:

- **Task metadata** (small, injected by orchestrator): goal, criteria, discovery summaries
- **Vault** (large, queried on demand): full research, analysis, failure records

### Discovery Flow

When an agent discovers that reality differs from assumptions:

1. Agent records full detail in vault (via Record)
2. Agent records a 1–3 sentence summary in its own task's `discoveries`
3. Parent runs inter-subtask checkpoint to classify the discovery
4. If the discovery affects parent scope, it bubbles up through the task tree

Vault owns step 1 (persistent, queryable storage). The orchestrator owns steps 2–4.

