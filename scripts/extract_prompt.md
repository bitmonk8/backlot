Follow these instructions exactly. Do not improvise or deviate. You may reason freely in your text output — the downstream script reads your result from a file, not from your text response.

## Task

Extract the **last issue** from `docs/ISSUES.md`, validate it against the codebase, enrich it, and write structured JSON to `scripts/.extract_result.json`.

## Step 1: Find the last issue

IMPORTANT: Do NOT read the entire `docs/ISSUES.md` file. The file is {{LINE_COUNT}} lines long. Use the Read tool with `offset: {{TAIL_OFFSET}}` and `limit: 100` to read only the last 100 lines. If the chunk doesn't contain a complete issue, read the preceding 100 lines for more context. Never read the entire file.

Scan backward from the end to find the last issue entry. Issues are identified by `###` headings (e.g., `### 13. CacheRetention::Long TTL format...`) or table rows within grouped sections. Some issues are inside `| # | File | ... |` tables under a `### Group N` heading — each table row is a separate issue.

If the file contains only the top-level `# ` heading, section `## ` headings, or no issue entries at all, write to `scripts/.extract_result.json`:
```json
{"status": "empty"}
```
Then stop.

If the last issue is marked as resolved (~~strikethrough~~ on the heading), skip it — treat it as resolved.

## Step 2: Validate against the codebase

Read the source file(s) referenced by the issue using the Read tool. Check whether the issue still exists at or near the stated line numbers (lines may have shifted).

- If the issue **still exists**: proceed to Step 3.
- If the referenced code **no longer exists** or the issue has been fixed, write to `scripts/.extract_result.json`:
  ```json
  {"status": "resolved", "title": "<short title from the issue>"}
  ```
  Then stop.
- If the issue was **never valid** (false positive — the code doesn't exhibit the described problem), write to `scripts/.extract_result.json`:
  ```json
  {"status": "false_positive", "title": "<short title from the issue>"}
  ```
  Then stop.

## Step 3: Enrich and write result

Assign exactly 4 labels from the taxonomy below. Then compose the issue body and write the full JSON object to `scripts/.extract_result.json`.

### Label Taxonomy

**Crate** (assign exactly 1):
`crate:flick`, `crate:lot`, `crate:reel`, `crate:vault`, `crate:epic`, `crate:mech`

**Importance** (assign exactly 1 — how much does this issue matter?):
- `importance:low` — cosmetic, negligible practical impact
- `importance:medium` — causes confusion, fragility, or moderate maintenance burden
- `importance:high` — correctness risk, security issue, or blocks other work

**Effort** (assign exactly 1 — how hard is the fix?):
- `effort:low` — <30 min, <50 LOC, minimal risk
- `effort:medium` — 1-4 hours, touches multiple files, some test effort
- `effort:high` — >4 hours, architectural change, significant test effort

**Type** (assign exactly 1):
- `type:bug` — Correctness or error handling issue
- `type:testing` — Missing or inadequate test coverage
- `type:security` — Security vulnerability or risk
- `type:performance` — Performance issue
- `type:complexity` — Unnecessary complexity, duplication, or separation of concerns issue
- `type:naming` — Misleading or inconsistent naming
- `type:docs` — Documentation inaccuracy, stale comments, cruft
- `type:placement` — Code in the wrong file or module

### Issue body format

Compose the body as GitHub-flavored markdown following this structure:

```
## File(s)

`path/to/file.rs` (lines X-Y)

## Issue

Description of the problem — what is wrong and why it matters.
Be specific. Reference the actual code you read.

## Impact

Descriptive paragraph(s) explaining specific consequences.
Not a scalar label — full sentences about what goes wrong if this isn't fixed.

## Fix Cost

- **Risk:** Specific regression paths this fix could trigger.
- **Effort:** Concrete estimate — LOC, files, test cases.
- **Maintenance burden:** Net LOC change, new abstractions or invariants introduced.
```

If the issue was part of a named group (e.g., "Group 16 — Duplicated platform code patterns"), include the group name at the top of the Issue section for context.

### JSON schema

Write exactly this structure to `scripts/.extract_result.json`:

```json
{
  "status": "valid",
  "title": "<concise issue title, max 80 chars>",
  "labels": ["crate:xxx", "importance:xxx", "effort:xxx", "type:xxx"],
  "body": "<full markdown body as a single string>"
}
```

The `title` should be descriptive and standalone — someone reading just the title in a GitHub issue list should understand what the issue is about.

You MUST write the JSON to `scripts/.extract_result.json` using the Write tool before finishing.
