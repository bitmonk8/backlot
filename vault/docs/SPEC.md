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

pub enum VaultCreateError {
    /// Storage root does not exist or is inaccessible.
    StorageUnavailable(PathBuf),
}
```

`model_registry` and `provider_registry` are required because the librarian is a reel agent that makes LLM calls internally. `models` allows the orchestrator to choose which model handles each operation — e.g., Haiku for lightweight operations like Query and Record, a larger model for Bootstrap or Reorganize if needed.

---

## Storage

Centralized knowledge at a configurable root directory (e.g., `.epic/docs/`). All consumers see all accumulated knowledge, organized by topic. File-based (markdown). Small document counts make this sufficient.

### Directory Structure

The storage root is partitioned into three areas:

```
storage_root/
├── CHANGELOG.md          # Append-only mutation log (vault-managed)
├── raw/                  # Client-provided content (vault-managed)
│   ├── REQUIREMENTS_1.md
│   ├── FINDINGS_1.md
│   ├── FINDINGS_2.md
│   └── ...
└── derived/              # Librarian-produced documents
    ├── PROJECT.md
    ├── DESIGN.md
    └── ...
```

| Area | Writer | Librarian access | Content |
|---|---|---|---|
| `raw/` | Vault (programmatic) | Read-only | Raw inputs: bootstrap requirements, record contents. Immutable once written. |
| `derived/` | Librarian | Read-write | Organized, curated documents. Current-reality view of project knowledge. |
| `CHANGELOG.md` | Vault (programmatic) | Read-only | Append-only log of all mutations. |

**Recoverability:** If the librarian corrupts derived documents, the entire `derived/` directory can be reconstructed from `CHANGELOG.md` and `raw/` contents. The raw data and changelog together form the authoritative record; derived documents are a curated view.

### Raw Documents

Raw documents are versioned by name. Each document has a base name and a version number: `NAME_N.md` (e.g., `FINDINGS_1.md`, `FINDINGS_2.md`). Later versions supersede earlier versions. The full version history is preserved — raw documents are never modified or deleted.

The version number is assigned by vault at write time. See the [Record operation](#3-record) for details.

### Core Principle

Documents in `derived/` describe **current reality**. They are not logs, journals, or histories. When information becomes obsolete — a decision is reversed, a constraint is resolved, an approach is abandoned — the old information is **removed** from the document. The document should read as if it were written today. All historical changes are captured in CHANGELOG.md, which is managed programmatically by vault (not by the librarian).

### Document Format

**Naming:** `UPPERCASE_DESCRIPTIVE` names. In `derived/`: `FINDINGS.md`, `API_DESIGN.md`, `MIGRATION_PLAN.md`. In `raw/`: versioned as `NAME_N.md` (e.g., `FINDINGS_1.md`). No lowercase.

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

When creating a new document in `derived/`: choose an `UPPERCASE_DESCRIPTIVE` name, add the standard header, add appropriate section types, and update PROJECT.md to index the new document.

## Core Documents

**Raw (created at bootstrap):**

| Document | Purpose |
|---|---|
| `raw/REQUIREMENTS_1.md` | Initial requirements as provided to bootstrap |

**Derived (created by librarian at bootstrap):**

| Document | Purpose |
|---|---|
| `derived/PROJECT.md` | Project overview + document index |
| `derived/REQUIREMENTS.md` | Structured requirements (derived from raw) |
| `derived/FINDINGS.md` | Accumulated discoveries |
| `derived/DESIGN.md` | Design decisions |
| Topic-specific | Created as needed by librarian in `derived/` |

**Root:**

| Document | Purpose |
|---|---|
| `CHANGELOG.md` | Append-only mutation log (managed programmatically, not by librarian) |

The set of core derived documents is fixed at bootstrap time. Topic-specific documents are created dynamically by the librarian in `derived/` as the knowledge base grows.

---

## Operations

### 1. Bootstrap

Convert an initial set of requirements into the core document set. This runs once, before any interactive session.

```rust
pub async fn bootstrap(&self, requirements: &str) -> Result<(), BootstrapError>;

pub enum BootstrapError {
    /// Bootstrap called on an already-initialized vault.
    AlreadyInitialized,
    Io(std::io::Error),
    LibrarianFailed(String),
}
```

Input: raw requirements text. Output: populated core documents on disk. Returns error if the storage root already contains core documents (bootstrap is not idempotent).

**Sequence:**
1. Vault writes the raw requirements to `raw/REQUIREMENTS_1.md`.
2. Vault invokes the librarian to produce the core derived documents in `derived/`.
3. Vault appends a bootstrap entry to `CHANGELOG.md`.

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

pub async fn query(&self, question: &str) -> Result<QueryResult, QueryError>;

pub enum QueryError {
    Io(std::io::Error),
    LibrarianFailed(String),
}
```

Design choice: vault does not define a "not found" error for Query. A `QueryResult` with `Coverage::None` is the normal response when no documents match — not an error condition.

`coverage` is the librarian's structured assessment of how well the vault's existing knowledge answers the question. The Research Service uses this to decide whether further research is needed without parsing the natural-language answer. `answer` is a consolidated response suitable for printing to end users — it may be partial if coverage is `Partial`. `extracts` provide source material for validation and deeper reading.

The orchestrator's Research Service layers on top of this: it calls `query`, checks `coverage` to decide whether gaps exist, fills gaps via external tools if needed, calls `record` to store new findings, then returns its own `ResearchResult` to the caller. Vault is unaware of the Research Service — it only provides storage and retrieval.

### 3. Record

Write content into vault. Vault stores the raw content, then the librarian integrates it into derived documents.

```rust
pub enum RecordMode {
    /// Create a new document series. Fails if any version already exists.
    New,
    /// Append a new version to an existing document series.
    Append,
}

pub async fn record(
    &self,
    name: &str,
    content: &str,
    mode: RecordMode,
) -> Result<Vec<DocumentRef>, RecordError>;

pub enum RecordError {
    /// Name is not UPPERCASE_DESCRIPTIVE or contains a version suffix.
    InvalidName(String),
    /// RecordMode::New but versions already exist.
    VersionConflict(String),
    /// RecordMode::Append but no prior version exists.
    DocumentNotFound(String),
    Io(std::io::Error),
    LibrarianFailed(String),
}
```

**Parameters:**
- `name`: Document base name (e.g., `"FINDINGS"`). Must be `UPPERCASE_DESCRIPTIVE` format (uppercase letters and underscores only) and must not contain a trailing version number (e.g., `"FINDINGS_2"` is rejected). Returns `RecordError::InvalidName` if validation fails. Vault stores the content in `raw/` as `NAME_N.md` where N is the next version number.
- `content`: Raw content (findings, decisions, discoveries).
- `mode`: Controls version behavior:
  - `RecordMode::New` — Creates version 1. Returns `RecordError::VersionConflict` if any version of this document already exists in `raw/`.
  - `RecordMode::Append` — Creates the next version (e.g., if `FINDINGS_2.md` is the latest, creates `FINDINGS_3.md`). Returns `RecordError::DocumentNotFound` if no prior version exists.

**Sequence:**
1. Vault determines the next version number by scanning `raw/` for existing `NAME_*.md` files.
2. Vault writes the content to `raw/NAME_N.md`.
3. Vault invokes the librarian to integrate the new content into `derived/` documents. The librarian reads the new raw document and updates derived documents accordingly.
4. Vault appends a record entry to `CHANGELOG.md`.

**Output:** References to derived documents that were created or modified.

**Later versions supersede earlier versions.** When the librarian processes a new raw document, it treats the content as the latest truth for that document series. The librarian can read all versions in `raw/` to understand evolution, but the latest version takes precedence.

**Relevance filter:** The librarian does not blindly copy raw content into derived documents. It filters for information that a future task would need to: avoid repeating failed work, make consistent decisions, understand constraints or blockers, or follow established patterns. Raw error logs, intermediate build output, routine progress updates, and information only relevant to the current task's execution are summarized or discarded.

**No restructuring during Record.** The Record operation only adds or updates content relevant to the new information. It does not reorder sections, split documents, or restructure for cosmetic reasons. Restructuring is reserved for the Reorganize operation.

### 4. Reorganize

Trigger a thorough restructuring pass over the entire vault. The librarian reviews all documents for merging opportunities, structural improvements, and deduplication. Unlike the lightweight librarian pass on each Record, this is a full sweep.

```rust
pub async fn reorganize(&self) -> Result<ReorganizeReport, ReorganizeError>;

pub enum ReorganizeError {
    Io(std::io::Error),
    LibrarianFailed(String),
}

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

A reel agent that manages document organization. The model for each operation is configured via `VaultModels`. Invoked internally by vault on Bootstrap, Query, Record, and Reorganize. Not exposed to external callers.

**Responsibilities:**
- **Placement**: decides which file and section in `derived/` receives new content.
- **Merging**: combines related content across derived documents when overlap grows.
- **Restructuring**: reorganizes sections as topics evolve.
- **Deduplication**: prevents repeated information.
- **Growth control**: prevents unbounded document expansion.

**Tool access:** The librarian has read access to `raw/` and `CHANGELOG.md`, and read-write access to `derived/`. It cannot modify or delete raw documents or the changelog. It has no access to tools outside the storage root (no codebase exploration, no web search, no shell commands). The constraint is **scope**: it operates exclusively within the vault storage directory, with write access limited to `derived/`.

### System Prompt Composition

The librarian's system prompt is composed per-operation from shared building blocks and operation-specific instructions. This avoids the duplication problem of maintaining separate guide files with overlapping content (as the Python epic predecessor did with META.md / GUIDE_READ.md / GUIDE_WRITE.md / GUIDE_MAINTAIN.md).

**Shared blocks** (included in every operation's prompt):

| Block | Content |
|---|---|
| Core principle | "Documents describe current reality." Superseding rules. |
| Document format | UPPERCASE_DESCRIPTIVE naming, standard header, typed sections. |
| Cross-references | Path reference format, no content duplication. |
| Scope restriction | Read `raw/` and `CHANGELOG.md`. Read-write `derived/`. No external tools. |
| Document inventory | Current list of documents in `raw/` and `derived/` with their scope comments. |

**Operation-specific blocks:**

| Operation | Additional blocks |
|---|---|
| Query | Extraction process: start with `derived/PROJECT.md` for orientation, read relevant documents in `derived/` and `raw/`, synthesize answer with coverage assessment. No writes. |
| Record | New raw document path provided. Relevance filter, placement rules (read `derived/PROJECT.md` for index, identify target document and section in `derived/`). Superseding rules (replace, don't append). No restructuring. |
| Reorganize | Full restructuring operations on `derived/`: split, merge, remove, consolidate, reorder, tighten prose. May read `raw/` for source of truth. Document lifecycle triggers (size ~200 lines, coherence). Update `derived/PROJECT.md` after structural changes. |
| Bootstrap | Raw requirements path provided. Core document templates (`derived/PROJECT.md`, `derived/REQUIREMENTS.md`). Initialization rules: derive structure from raw requirements, don't invent structure they don't support. |

Vault assembles the final prompt by concatenating the relevant blocks. The shared blocks are defined once as embedded constants (e.g., `include_str!`). The document inventory block is generated at call time from the current state of the storage root.

This scoping prevents the librarian from restructuring documents during a Record call, or from modifying documents during a Query call — not by convention, but because the instructions for those capabilities are absent from the prompt.

---

## CLI

The `vault-cli` crate provides a command-line interface that exposes vault's public API as subcommands. Following the conventions of sibling projects (reel, lot, flick): clap 4 with derive macros, YAML configuration, JSON output to stdout, errors to stderr, tokio single-threaded runtime.

### Configuration

The CLI reads a YAML config file that maps to `VaultEnvironment`. The CLI constructs the model and provider registries internally — these are embedding-only plumbing not exposed as CLI arguments.

```yaml
storage_root: ".epic/docs/"
models:
  bootstrap: "sonnet"
  query: "haiku"
  record: "haiku"
  reorganize: "sonnet"
```

The `--config <path>` flag is required for all subcommands that interact with the vault.

### Subcommands

#### `vault bootstrap`

```
vault bootstrap --config <path>
```

Reads requirements from stdin. Creates the initial vault structure. Maps to `Vault::bootstrap()`.

#### `vault query`

```
vault query --config <path> --query <text>
vault query --config <path> < question.txt
```

Query text via `--query` flag or stdin. Outputs `QueryResult` as JSON to stdout. Maps to `Vault::query()`.

#### `vault record`

```
vault record --config <path> --name <NAME> --mode new|append
vault record --config <path> --name <NAME> --mode new|append --content <text>
```

Content via `--content` flag or stdin. `--name` is the document base name (e.g., `FINDINGS`). `--mode` is required. Outputs the list of modified `DocumentRef`s as JSON. Maps to `Vault::record()`.

#### `vault reorganize`

```
vault reorganize --config <path>
```

Triggers a full restructuring pass. Outputs `ReorganizeReport` as JSON. Maps to `Vault::reorganize()`.

### Output

All subcommands emit JSON to stdout on success. Errors are emitted as JSON to stderr with a non-zero exit code. This matches the reel and flick convention of machine-readable output for composability.

---

## Integration Contract

Vault is a library consumed by orchestrators (e.g., epic). It provides Bootstrap, Query, Record, and Reorganize as its public API. Higher-level services (e.g., epic's Research Service) layer on top by calling these operations combined with external tools.

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

