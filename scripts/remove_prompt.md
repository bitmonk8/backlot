Follow these instructions exactly. Do not improvise or deviate.

## Task

Remove the **last issue entry** from `docs/ISSUES.md`. Preserve all other content.

## Steps

1. Read the last 80 lines of `docs/ISSUES.md` using the Read tool (use the `offset` parameter to read only the tail).

2. Identify the last issue entry. Issues take one of these forms:
   - A `###` heading followed by content, terminated by `---` or end of file.
   - A table row (`| # | File | ... |`) inside a `### Group N` section — remove only the last table row, not the entire group. If removing the row leaves the table with only a header row and separator, remove the entire table and its `### Group` heading.

3. Use the Edit tool to remove the identified content. Be precise:
   - Remove from the start of the issue entry through its `---` separator (inclusive) or end of file.
   - If the issue is a table row, remove only that row.
   - Do not leave trailing blank lines beyond what was already there.
   - Do not modify any other part of the file.

4. If the last entry was the only issue under a `## Crate` section, and removing it leaves the section empty (just the `## Crate` heading with no content below it before the next `---` or `## ` heading), remove the empty section heading and its `---` separator too.

Do not output anything. Just perform the edit.
