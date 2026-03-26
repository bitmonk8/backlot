// System prompt composition for librarian invocations.
//
// Shared blocks (core principle, document format, cross-references, scope
// restriction, document inventory) are combined with operation-specific blocks
// to produce the final system prompt for each vault operation.

use crate::storage::{DocumentInventory, Storage, StorageError};

use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Shared prompt blocks (included in every librarian invocation)
// ---------------------------------------------------------------------------

const CORE_PRINCIPLE: &str = "\
## Core Principle

Documents describe current reality. When new information conflicts with \
existing documents, the new information wins. Update documents to reflect \
the latest state; do not preserve outdated content.";

const DOCUMENT_FORMAT: &str = "\
## Document Format

- Filenames: `UPPERCASE_DESCRIPTIVE.md` (e.g., `PROJECT.md`, `REQUIREMENTS.md`).
- Header: First line is `# Title`. Second line is `<!-- scope: brief description -->`.
- Sections: Use `##` headings. Each section has a clear purpose.
- Content: Concise, factual, no filler. Use bullet lists for enumerations.";

const CROSS_REFERENCES: &str = "\
## Cross-References

- Reference other documents by path: `See [DESIGN.md](DESIGN.md)`.
- Do not duplicate content across documents. Reference instead.";

const SCOPE_RESTRICTION: &str = "\
## Scope Restriction

You may read files in `raw/` and `CHANGELOG.md`. \
You may read and write files in `derived/`. \
Do not access any files outside these locations. \
Do not use network tools or shell commands.";

fn document_inventory_block(inventory: &DocumentInventory) -> String {
    let mut block = String::from("## Current Document Inventory\n\n");

    if inventory.raw.is_empty() && inventory.derived.is_empty() {
        block.push_str("No documents exist yet.\n");
        return block;
    }

    if !inventory.raw.is_empty() {
        block.push_str("### Raw Documents\n\n");
        for doc in &inventory.raw {
            let _ = writeln!(block, "- `raw/{}`", doc.filename);
        }
        block.push('\n');
    }

    if !inventory.derived.is_empty() {
        block.push_str("### Derived Documents\n\n");
        for doc in &inventory.derived {
            if let Some(scope) = &doc.scope {
                let _ = writeln!(block, "- `derived/{}` — {}", doc.filename, scope);
            } else {
                let _ = writeln!(block, "- `derived/{}`", doc.filename);
            }
        }
        block.push('\n');
    }

    block
}

fn build_shared_prompt(inventory: &DocumentInventory, scope_restriction: &str) -> String {
    format!(
        "{CORE_PRINCIPLE}\n\n{DOCUMENT_FORMAT}\n\n{CROSS_REFERENCES}\n\n{scope_restriction}\n\n{}",
        document_inventory_block(inventory)
    )
}

// ---------------------------------------------------------------------------
// Bootstrap-specific prompt
// ---------------------------------------------------------------------------

const BOOTSTRAP_BLOCK: &str = "\
## Bootstrap Task

You are initializing a new vault from raw requirements.

The raw requirements have been written to `raw/REQUIREMENTS_1.md`. Read that file.

### Required Output

Create the following core derived documents in `derived/`:

1. **`derived/PROJECT.md`** - Project overview and index. Summarize what the project is, \
its goals, and reference other derived documents.

2. **`derived/REQUIREMENTS.md`** - Structured requirements extracted from the raw input. \
Organize into clear categories. Each requirement should be actionable and testable.

### Additional Documents

If the requirements cover distinct topics that warrant separate documents, create additional \
`derived/TOPIC_NAME.md` files. Only create additional documents when the requirements clearly \
support them. Do not invent structure the requirements do not warrant.

### Rules

- Derive all content from the raw requirements. Do not invent information.
- Every derived document must follow the document format (title + scope header).
- Use cross-references between documents where appropriate.
- Keep documents focused and non-overlapping.";

fn bootstrap_system_prompt(inventory: &DocumentInventory) -> String {
    format!(
        "{}\n\n{BOOTSTRAP_BLOCK}",
        build_shared_prompt(inventory, SCOPE_RESTRICTION)
    )
}

// ---------------------------------------------------------------------------
// Record-specific prompt
// ---------------------------------------------------------------------------

const RECORD_BLOCK: &str = "\
## Record Task

A new raw document has been written to `raw/RAW_FILENAME_PLACEHOLDER`.

### Instructions

1. Read `derived/PROJECT.md` for orientation and the current document index.
2. Read the new raw document listed above.
3. Integrate the new content into existing derived documents or create new ones as warranted.
4. Apply the relevance filter: keep information useful for future tasks (decisions, \
constraints, patterns, failure records). Discard routine progress and intermediate output.
5. Follow the superseding rule: new information replaces outdated information in derived docs.
6. Do not restructure existing documents. Only add or update content relevant to the new information.
7. If you create new derived documents, update the index in `derived/PROJECT.md`.";

fn record_system_prompt(inventory: &DocumentInventory, raw_filename: &str) -> String {
    format!(
        "{}\n\n{}",
        build_shared_prompt(inventory, SCOPE_RESTRICTION),
        RECORD_BLOCK.replace("RAW_FILENAME_PLACEHOLDER", raw_filename)
    )
}

// ---------------------------------------------------------------------------
// Query-specific prompt
// ---------------------------------------------------------------------------

const QUERY_SCOPE_RESTRICTION: &str = "\
## Scope Restriction

You may read files in `raw/`, `derived/`, and `CHANGELOG.md`. \
You must NOT write, create, edit, or delete any files. \
Do not use network tools or shell commands.";

const QUERY_BLOCK: &str = "\
## Query Task

Answer the user's question using the vault's documents.

### Extraction Process

1. Read `derived/PROJECT.md` for orientation and the current document index.
2. Read relevant documents in `derived/` and `raw/` based on the question.
3. Synthesize an answer from the documents.
4. Assess coverage: `full` if the documents completely answer the question, \
`partial` if they partially answer it, `none` if they contain no relevant information.

### Rules

- Do NOT write, create, edit, or delete any files.
- Base your answer only on the vault's documents. Do not invent information.
- Include extracts (verbatim or closely paraphrased passages) that support your answer.

### Required Output Format

Return a single JSON object (no other text) with this structure:

```json
{
  \"coverage\": \"full\" | \"partial\" | \"none\",
  \"answer\": \"<your synthesized answer>\",
  \"extracts\": [
    {\"content\": \"<supporting passage>\", \"source\": \"<filename>\"},
    ...
  ]
}
```

The `source` field is the filename only (e.g., `PROJECT.md`), not the full path.";

fn query_system_prompt(inventory: &DocumentInventory) -> String {
    format!(
        "{}\n\n{QUERY_BLOCK}",
        build_shared_prompt(inventory, QUERY_SCOPE_RESTRICTION)
    )
}

// ---------------------------------------------------------------------------
// Reorganize-specific prompt
// ---------------------------------------------------------------------------

const REORGANIZE_BLOCK: &str = "\
## Reorganize Task

Full restructuring operations on `derived/`: split, merge, remove, consolidate, \
reorder, tighten prose. May read `raw/` for source of truth.

### Instructions

1. Read `derived/PROJECT.md` for orientation and the current document index.
2. Review all derived documents for restructuring opportunities.
3. Apply document lifecycle triggers:
   - **Size**: documents approaching ~200 lines should be split into focused subtopics.
   - **Coherence**: documents covering multiple unrelated topics should be split.
   - **Overlap**: documents with duplicated content should be merged or deduplicated.
4. Merge related or overlapping documents.
5. Split documents that cover too many topics.
6. Remove duplicated content across documents.
7. Tighten prose: eliminate filler, reduce verbosity, sharpen facts.
8. Update `derived/PROJECT.md` index after structural changes.

### Rules

- Derive all content from existing documents. Do not invent information.
- Every derived document must follow the document format (title + scope header).
- Use cross-references between documents where appropriate.
- Keep documents focused and non-overlapping.
- May read `raw/` to verify content accuracy.";

fn reorganize_system_prompt(inventory: &DocumentInventory) -> String {
    format!(
        "{}\n\n{REORGANIZE_BLOCK}",
        build_shared_prompt(inventory, SCOPE_RESTRICTION)
    )
}

// ---------------------------------------------------------------------------
// Public access for operation modules
// ---------------------------------------------------------------------------

/// Bootstrap user message sent as the user turn to the librarian.
pub const fn bootstrap_user_message() -> &'static str {
    "Initialize this vault. Read the raw requirements and create the core derived documents."
}

/// Build the system prompt for a bootstrap invocation from current storage state.
pub fn build_bootstrap_prompt(storage: &Storage) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(bootstrap_system_prompt(&inventory))
}

/// Record user message sent as the user turn to the librarian.
pub const fn record_user_message() -> &'static str {
    "Integrate the new raw document into the derived knowledge base."
}

/// Build the system prompt for a record invocation from current storage state.
pub fn build_record_prompt(storage: &Storage, raw_filename: &str) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(record_system_prompt(&inventory, raw_filename))
}

/// Format the user's question for the query librarian invocation.
pub fn query_user_message(question: &str) -> String {
    format!("Answer this question from the vault's knowledge base:\n\n{question}")
}

/// Build the system prompt for a query invocation from current storage state.
pub fn build_query_prompt(storage: &Storage) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(query_system_prompt(&inventory))
}

/// Reorganize user message sent as the user turn to the librarian.
pub const fn reorganize_user_message() -> &'static str {
    "Perform a full restructuring pass over the derived knowledge base."
}

/// Build the system prompt for a reorganize invocation from current storage state.
pub fn build_reorganize_prompt(storage: &Storage) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(reorganize_system_prompt(&inventory))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::storage::Storage;
    use tempfile::TempDir;

    #[test]
    fn bootstrap_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let prompt = build_bootstrap_prompt(&storage).unwrap();

        assert!(prompt.contains("Core Principle"));
        assert!(prompt.contains("Document Format"));
        assert!(prompt.contains("Cross-References"));
        assert!(prompt.contains("Scope Restriction"));
        assert!(prompt.contains("Current Document Inventory"));
        assert!(prompt.contains("Bootstrap Task"));
        assert!(prompt.contains("PROJECT.md"));
        assert!(prompt.contains("REQUIREMENTS.md"));
    }

    #[test]
    fn inventory_block_shows_existing_documents() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        storage
            .write_raw_versioned("REQUIREMENTS", "some reqs", true)
            .unwrap();
        std::fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project\n<!-- scope: overview -->\n",
        )
        .unwrap();

        let inventory = storage.inventory().unwrap();
        let block = document_inventory_block(&inventory);

        assert!(block.contains("raw/REQUIREMENTS_1.md"));
        assert!(block.contains("derived/PROJECT.md"));
        assert!(
            block.contains("derived/PROJECT.md` — overview"),
            "scope comment should appear next to its document"
        );
    }

    #[test]
    fn inventory_block_omits_scope_suffix_when_none() {
        let inventory = DocumentInventory {
            raw: vec![],
            derived: vec![
                crate::storage::DerivedDocumentInfo {
                    filename: "PROJECT.md".to_owned(),
                    scope: Some("overview".to_owned()),
                },
                crate::storage::DerivedDocumentInfo {
                    filename: "NOTES.md".to_owned(),
                    scope: None,
                },
            ],
        };
        let block = document_inventory_block(&inventory);

        assert!(
            block.contains("derived/PROJECT.md` — overview"),
            "should render scope when present"
        );
        assert!(
            block.contains("- `derived/NOTES.md`\n"),
            "should render without scope suffix when None"
        );
        assert!(
            !block.contains("NOTES.md` —"),
            "should not have dash suffix for None scope"
        );
    }

    #[test]
    fn empty_inventory_says_no_documents() {
        let inventory = DocumentInventory::default();
        let block = document_inventory_block(&inventory);
        assert!(block.contains("No documents exist yet"));
    }

    #[test]
    fn record_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        storage
            .write_raw_versioned("FINDINGS", "some findings", true)
            .unwrap();
        std::fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project\n<!-- scope: overview -->\n",
        )
        .unwrap();

        let prompt = build_record_prompt(&storage, "FINDINGS_1.md").unwrap();

        // Shared blocks
        assert!(prompt.contains("Core Principle"));
        assert!(prompt.contains("Document Format"));
        assert!(prompt.contains("Cross-References"));
        assert!(prompt.contains("Scope Restriction"));
        assert!(prompt.contains("Current Document Inventory"));

        // Record-specific block
        assert!(prompt.contains("Record Task"));
        assert!(prompt.contains("raw/FINDINGS_1.md"));
        assert!(prompt.contains("derived/PROJECT.md"));
        assert!(prompt.contains("relevance filter"));
        assert!(prompt.contains("superseding rule"));
    }

    #[test]
    fn record_prompt_includes_raw_filename() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let prompt = build_record_prompt(&storage, "NOTES_3.md").unwrap();
        assert!(prompt.contains("raw/NOTES_3.md"));
    }

    #[test]
    fn query_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        std::fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project\n<!-- scope: overview -->\n",
        )
        .unwrap();

        let prompt = build_query_prompt(&storage).unwrap();

        // Shared blocks
        assert!(prompt.contains("Core Principle"));
        assert!(prompt.contains("Document Format"));
        assert!(prompt.contains("Cross-References"));
        assert!(prompt.contains("Current Document Inventory"));

        // Query-specific scope restriction (read-only)
        assert!(prompt.contains("must NOT write"));

        // Query-specific block
        assert!(prompt.contains("Query Task"));
        assert!(prompt.contains("derived/PROJECT.md"));
        assert!(prompt.contains("coverage"));
        assert!(prompt.contains("extracts"));
    }

    #[test]
    fn query_user_message_includes_question() {
        let msg = query_user_message("What is the project about?");
        assert!(msg.contains("What is the project about?"));
    }

    #[test]
    fn reorganize_prompt_contains_required_sections() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        std::fs::write(
            storage.derived_dir().join("PROJECT.md"),
            "# Project\n<!-- scope: overview -->\n",
        )
        .unwrap();

        let prompt = build_reorganize_prompt(&storage).unwrap();

        // Shared blocks
        assert!(prompt.contains("Core Principle"));
        assert!(prompt.contains("Document Format"));
        assert!(prompt.contains("Cross-References"));
        assert!(prompt.contains("Scope Restriction"));
        assert!(prompt.contains("Current Document Inventory"));

        // Reorganize-specific block
        assert!(prompt.contains("Reorganize Task"));
        assert!(prompt.contains("lifecycle triggers"));
        assert!(prompt.contains("200 lines"));
        assert!(prompt.contains("PROJECT.md"));
    }
}
