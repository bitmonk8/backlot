# Backlot Monorepo Migration

## Why

Five sibling projects (epic, reel, lot, vault, flick) currently live in separate repos under `bitmonk8`, referencing each other via pinned git rev dependencies. Problems:

- **Version drift**: lot is pinned at `f131ad9` in epic but `30bd25f` in reel.
- **Cross-crate changes** require multi-repo commit-push-bump cycles.
- **CI duplication**: 5 nearly identical GitHub Actions configs.
- **Cognitive overhead**: switching repos to trace a bug across crate boundaries.

A Cargo workspace monorepo eliminates all four.

## Repo

- **Name**: `backlot`
- **GitHub**: `github.com/bitmonk8/backlot`
- **Git user**: `Thomas Andersen <thomas.andersen@gmail.com>`

## Current Dependency Graph

```
flick  (leaf — no project deps)
lot    (leaf — no project deps)
reel   → flick, lot
vault  → reel
epic   → lot, reel, vault
```

## Target Structure

```
backlot/
├── Cargo.toml                  # workspace root
├── .github/workflows/ci.yml   # unified CI
├── .gitattributes
├── .gitignore
├── CLAUDE.md                   # merged project-assistant directives
├── backlot_project_assistant.nu
├── backlot_shell.nu
├── prompts/
│   └── project_assistant.md    # unified system prompt
├── flick/
│   ├── flick/                  # library crate
│   └── flick-cli/              # binary crate
├── lot/
│   ├── lot/                    # library crate
│   └── lot-cli/                # binary crate
├── reel/
│   ├── reel/                   # library crate
│   └── reel-cli/               # binary crate
├── vault/
│   ├── vault/                  # library crate
│   └── vault-cli/              # binary crate
└── epic/                       # binary crate (currently no sub-crates)
```

Each project directory retains its internal structure (docs/, tests, etc.). Per-project workspace Cargo.toml files (in reel, lot, vault, flick) are removed — the root workspace replaces them. Per-project shell scripts and system prompts are replaced by a single set at the monorepo root.

## Migration Steps

### Step 1: Create repo and merge histories

Uses `git-filter-repo` to rewrite each project's history so files appear under their subdirectory, then merges all into backlot. Result: `git log flick/flick/src/lib.rs` traces back to the original commits.

```bash
# For each project, create a rewritten clone:
for proj in flick lot reel vault epic; do
  git clone git@github.com:bitmonk8/$proj.git /tmp/$proj-rewrite
  cd /tmp/$proj-rewrite
  git filter-repo --to-subdirectory-filter $proj
  cd -
done

# Then merge all into backlot:
cd /c/UnitySrc/backlot
git init
git config user.email "thomas.andersen@gmail.com"
git config user.name "Thomas Andersen"
git commit --allow-empty -m "Initialize backlot monorepo"

for proj in flick lot reel vault epic; do
  git remote add $proj /tmp/$proj-rewrite
  git fetch $proj
  git merge $proj/main --allow-unrelated-histories --no-edit \
    -m "Merge $proj history into monorepo"
  git remote remove $proj
done
```

### Step 2: Remove per-project workspace files

Delete these files (they're replaced by the root workspace):
- `flick/Cargo.toml` (workspace root)
- `lot/Cargo.toml` (workspace root)
- `reel/Cargo.toml` (workspace root)
- `vault/Cargo.toml` (workspace root)

Each project's library and CLI crates keep their own `Cargo.toml`.

### Step 3: Create root workspace Cargo.toml

```toml
[workspace]
members = [
    "flick/flick",
    "flick/flick-cli",
    "lot/lot",
    "lot/lot-cli",
    "reel/reel",
    "reel/reel-cli",
    "vault/vault",
    "vault/vault-cli",
    "epic",
]
resolver = "3"

[workspace.package]
edition = "2024"
rust-version = "1.85"

[workspace.lints.rust]
unsafe_code = "deny"

[workspace.lints.clippy]
all = "deny"
```

### Step 4: Convert git rev dependencies to path dependencies

| Crate Cargo.toml | Dependency | Old | New |
|---|---|---|---|
| `epic/Cargo.toml` | lot | `git rev f131ad9` | `path = "../lot/lot"` |
| `epic/Cargo.toml` | reel | `git rev 93f35ef` | `path = "../reel/reel"` |
| `epic/Cargo.toml` | vault | `git rev f7ecea1` | `path = "../vault/vault"` |
| `reel/reel/Cargo.toml` | flick | `git rev 8b11845` | `path = "../../flick/flick"` |
| `reel/reel/Cargo.toml` | lot | `git rev 30bd25f` | `path = "../../lot/lot"` |
| `vault/vault/Cargo.toml` | reel | `git rev 93f35ef` | `path = "../../reel/reel"` |
| `vault/vault-cli/Cargo.toml` | reel | `git rev 93f35ef` | `path = "../../reel/reel"` |

This resolves the lot version mismatch — one copy of each crate, always at HEAD.

### Step 5: Inherit workspace settings in member crates

Each member crate's Cargo.toml can inherit from the workspace:

```toml
[package]
edition.workspace = true
rust-version.workspace = true

[lints]
workspace = true
```

This replaces per-crate edition/lints declarations.

### Step 6: Unified CI

Single `.github/workflows/ci.yml` replacing 5 configs. Key considerations:
- `cargo fmt --all --check` (workspace-wide)
- `cargo clippy --workspace -- -D warnings` (all platforms)
- Per-crate test jobs where platform setup differs:
  - **lot/reel**: Linux needs `sysctl kernel.unprivileged_userns_clone=1`, Windows needs AppContainer setup
  - **flick/vault/epic**: Standard `cargo test -p <crate>` on all platforms
- `cargo build --workspace` (all platforms)

### Step 7: Merge configuration and shell scripts

#### Configuration files

- `.gitattributes`: All projects use `* text=auto eol=lf`. One file.
- `.gitignore`: Union of all project ignores. Remove per-project `.gitignore` files if redundant.
- `CLAUDE.md`: Merge common directives (testing policy, code style, etc.). Per-crate CLAUDE.md files can remain for crate-specific rules.

#### NuShell scripts and system prompt

All 5 projects have identical-pattern shell scripts (`*_project_assistant.nu`, `*_shell.nu`) and a `prompts/project_assistant.md` system prompt. Replace all of these with a single set at the monorepo root.

**Delete** (10 per-project scripts):
- `epic/epic_project_assistant.nu`, `epic/epic_shell.nu`
- `reel/reel_project_assistant.nu`, `reel/reel_shell.nu`
- `lot/lot_project_assistant.nu`, `lot/lot_shell.nu`
- `vault/vault_project_assistant.nu`, `vault/vault_shell.nu`
- `flick/flick_project_assistant.nu`, `flick/flick_shell.nu`

**Delete** (5 per-project prompts):
- `epic/prompts/project_assistant.md`
- `reel/prompts/project_assistant.md`
- `lot/prompts/project_assistant.md`
- `vault/prompts/project_assistant.md`
- `flick/prompts/project_assistant.md`

**Create** at monorepo root:
- `backlot_project_assistant.nu` — sources `~/claude-pilot-env.nu`, launches claude with `--append-system-prompt-file prompts/project_assistant.md "/new_assistant_session"`
- `backlot_shell.nu` — sources `~/claude-pilot-env.nu`, sets `BACKLOT_SHELL` env guard, re-execs with self as env config
- `prompts/project_assistant.md` — unified system prompt covering all 5 crates. Merges the shared structure (document maintenance, behavioral rules) with per-crate specifics (epic's external references, flick's PATH needs)

Note: flick's scripts additionally prepend `target/debug` to `$env.PATH`. This should be handled in the unified shell script (conditionally or unconditionally, depending on whether other crates need it).

### Step 8: Verify

1. `cargo build --workspace` succeeds
2. `cargo test --workspace` passes (246 epic tests + tests from other crates)
3. `cargo clippy --workspace -- -D warnings` clean
4. `cargo fmt --all --check` clean
5. CI green on all 3 platforms

### Step 9: Workspace dependency deduplication

Hoist shared dependencies into `[workspace.dependencies]` in the root `Cargo.toml`. Member crates reference them with `dep.workspace = true`.

```toml
# Root Cargo.toml
[workspace.dependencies]
tokio = { version = "1.42", features = ["full"] }
serde = { version = "1", features = ["derive"] }
# ... all shared deps
```

```toml
# Member Cargo.toml
[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
```

Process: grep all member `Cargo.toml` files, identify dependencies that appear in 2+ crates, hoist to workspace table, replace each occurrence. Re-run Step 8 verification after.

### Step 10: Push and archive

1. Create `github.com/bitmonk8/backlot` repo
2. `git remote add origin git@github.com:bitmonk8/backlot.git && git push -u origin main`
3. Archive each old repo on GitHub (Settings → Archive this repository)

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| History merge conflicts | filter-repo avoids conflicts — each project is in its own directory, no overlapping paths |
| lot version mismatch causes build failures | Resolve API differences when converting to path deps; lot HEAD should be a superset of both pinned revs |
| CI complexity | Start with one job per crate, optimize later |
| reel build.rs writes to `target/nu-cache/` | Workspace shares `target/` — verify nu binary caching still works with workspace target directory |
| Per-project shell scripts (`.nu` files) reference relative paths | Replaced by single set at monorepo root (Step 7) |

## Not In Scope (for now)

- Publishing crates to crates.io
- Changelog tooling (e.g., `cargo-release`, conventional commits)
