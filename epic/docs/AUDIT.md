# Audit Findings

## TOCTOU Races in File Operations

Mitigated by lot's per-phase process sandboxing. Partial code mitigations possible (e.g., `O_NOFOLLOW`, `flock`).

| Ref | Finding | Location |
|-----|---------|----------|
| U5-R1#1 | Symlink race in `safe_path` with `allow_new_file` — race between validation and open | `tools.rs` |
| U2-R2#2 | `write_file` path validated then file written non-atomically | `tools.rs` |
| U2-R2#3 | `edit_file` file may change between read and write | `tools.rs` |

See [DESIGN.md](DESIGN.md#operational-correctness-lot) for lot sandboxing details.
