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
            let _ = writeln!(block, "- `derived/{}`", doc.filename);
        }
        block.push('\n');
    }

    block
}

fn build_shared_prompt(inventory: &DocumentInventory) -> String {
    format!(
        "{CORE_PRINCIPLE}\n\n{DOCUMENT_FORMAT}\n\n{CROSS_REFERENCES}\n\n{SCOPE_RESTRICTION}\n\n{}",
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
    format!("{}\n\n{BOOTSTRAP_BLOCK}", build_shared_prompt(inventory))
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
        build_shared_prompt(inventory),
        RECORD_BLOCK.replace("RAW_FILENAME_PLACEHOLDER", raw_filename)
    )
}

// ---------------------------------------------------------------------------
// Public access for operation modules
// ---------------------------------------------------------------------------

/// Bootstrap query message sent as the user turn.
pub const fn bootstrap_query() -> &'static str {
    "Initialize this vault. Read the raw requirements and create the core derived documents."
}

/// Build the system prompt for a bootstrap invocation from current storage state.
pub fn build_bootstrap_prompt(storage: &Storage) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(bootstrap_system_prompt(&inventory))
}

/// Record query message sent as the user turn.
pub const fn record_query() -> &'static str {
    "Integrate the new raw document into the derived knowledge base."
}

/// Build the system prompt for a record invocation from current storage state.
pub fn build_record_prompt(storage: &Storage, raw_filename: &str) -> Result<String, StorageError> {
    let inventory = storage.inventory()?;
    Ok(record_system_prompt(&inventory, raw_filename))
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
}
