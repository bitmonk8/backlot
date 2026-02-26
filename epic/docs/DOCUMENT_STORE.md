# Document Store

## Purpose

Centralized knowledge collection at `.epic/docs/`. All tasks see all accumulated knowledge, organized by topic rather than by task. Acts as persistent memory across agent sessions.

## Design (Carried from fds2_epic)

### Core Documents

| Document | Purpose |
|---|---|
| EPIC.md | Project overview + document index |
| REQUIREMENTS.md | Captured from interactive session |
| CHANGELOG.md | Append-only mutation log |
| FINDINGS.md | Accumulated discoveries |
| DESIGN.md | Design decisions |
| Topic-specific | Created as needed by librarian |

### Operations

1. **Bootstrap** (pre-TUI): Convert requirements into initial document set
2. **Query**: Search documents for relevant knowledge. Returns extract + source references
3. **Record**: Write findings. Librarian decides file placement, merging, restructuring

### Librarian

A lightweight (Haiku) agent manages document placement, merging, and restructuring. Prevents documents from growing unbounded. Handles deduplication.

## Research Service

Exposed as a tool to calling agents. Interface:

```
research_query(question, scope) -> ResearchResult {
    answer: String,
    document_refs: Vec<String>,  // "FILENAME > Section" format
    gaps_filled: u32,
}
```

Scope: PROJECT (codebase exploration), WEB (web search), or BOTH.

Workflow:
1. Check DocumentStore for existing knowledge
2. Identify gaps
3. Fill gaps via codebase exploration or web search (Haiku)
4. Store results in DocumentStore
5. Return structured answer with provenance

The research service is demand-driven — called when an agent hits uncertainty during design or implementation, not as a mandatory preamble.

## Rust Implementation Considerations

- Document store is file-based (markdown files in `.epic/docs/`)
- Query/record operations invoke agent calls (Haiku) — needs ZeroClaw integration
- Consider whether ZeroClaw's built-in memory system (SQLite vector/keyword search) can serve as the query backend, with markdown files as the source of truth
- Serde for structured responses from librarian/query agents
