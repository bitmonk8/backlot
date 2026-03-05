# Flick — Library Extraction Spec

## Status

**Complete.** Phases 1-2 are done. The workspace restructure and `FlickClient` API are implemented.

Note: Implementation details below may differ from actual code — this document preserves the original spec for reference.

## Goal

Split Flick into a reusable Rust library crate (`flick`) and a thin CLI binary (`flick-cli`). Rust applications can embed Flick directly, avoiding process-spawn overhead and gaining connection/credential reuse across calls.

## Motivation

| Concern | CLI-only (status quo) | Library + CLI |
|---------|----------------------|---------------|
| Per-call overhead | Process spawn + config parse + credential decrypt + TLS handshake (~15-60ms) | Zero after first call |
| HTTP connection reuse | Impossible (process exits) | `reqwest::Client` kept alive across calls |
| Credential caching | Re-read and decrypt every invocation | Decrypt once, hold in memory |
| Cross-language use | Excellent (stdin/stdout) | Unchanged (CLI still exists) |
| Rust-native use | Shell out, parse JSON | Type-safe API, no serialization boundary |

The dominant performance win is **HTTP client reuse** (TLS session + connection pool). Credential caching and config parsing are smaller but real gains in tight agent loops (20+ round-trips).

## Crate Layout

```
flick/
  Cargo.toml          # workspace root (new)
  flick/              # library crate
    Cargo.toml        # [lib] — name = "flick"
    src/
      lib.rs          # public API surface
      config.rs
      context.rs
      credential.rs
      error.rs
      history.rs
      model.rs
      model_list.rs
      provider.rs
      provider/
        messages.rs
        chat_completions.rs
        http.rs
      result.rs
      runner.rs
  flick-cli/          # binary crate
    Cargo.toml        # [bin] — name = "flick", depends on flick
    src/
      main.rs         # arg parsing, stdin/stdout, interactive prompts
      prompter.rs     # TerminalPrompter (CLI-specific)
```

Alternative: single crate with `src/lib.rs` + `src/bin/flick.rs`. Simpler but mixes CLI dependencies (clap) into the library. **Workspace approach preferred** — keeps library dependency footprint minimal.

## Public API Design

### `FlickClient` — the reusable handle

```rust
/// Holds config, decrypted credentials, HTTP client, and provider.
/// Reuse across calls for maximum performance.
pub struct FlickClient { /* private fields */ }

impl FlickClient {
    /// Build from a parsed config. Decrypts credentials, creates HTTP
    /// client and provider. This is the expensive step — do it once.
    pub async fn new(config: Config) -> Result<Self, FlickError>;

    /// Build with a pre-existing reqwest::Client (for sharing across
    /// multiple FlickClients or other HTTP work).
    pub async fn with_http_client(
        config: Config,
        client: reqwest::Client,
    ) -> Result<Self, FlickError>;

    /// Single-shot query. Returns when the model responds.
    pub async fn run(&self, query: &str) -> Result<FlickResult, FlickError>;

    /// Resume a session with tool results.
    pub async fn resume(
        &self,
        context: &Context,
        tool_results: Vec<ToolResult>,
    ) -> Result<FlickResult, FlickError>;

    /// Build the API request body without sending it (dry-run).
    pub fn build_request(
        &self,
        query: &str,
    ) -> Result<serde_json::Value, FlickError>;
}
```

### Key types re-exported from `lib.rs`

```rust
// Config
pub use config::Config;

// Core types
pub use context::{Context, ContentBlock, Message, ToolResult};
pub use result::FlickResult;
pub use error::FlickError;

// Provider (for advanced use)
pub use provider::{ApiKind, Provider, DynProvider};

// Credential (for callers who want direct access)
pub use credential::CredentialStore;
```

### Config loading

```rust
impl Config {
    /// Parse from a YAML or JSON string. No file I/O.
    pub fn from_str(s: &str, format: ConfigFormat) -> Result<Self, ConfigError>;

    /// Load from a file path (existing behavior).
    pub async fn load(path: &Path) -> Result<Self, ConfigError>;
}

pub enum ConfigFormat {
    Yaml,
    Json,
}
```

`from_str` lets library users construct configs from embedded strings, environment variables, or any non-file source without touching the filesystem.

### Credential bypass

Library callers may already have API keys from their own secret management. The client should accept pre-resolved credentials:

```rust
pub struct ResolvedCredential {
    pub api_key: String,
    pub api_kind: ApiKind,
    pub base_url: Option<String>,
}

impl FlickClient {
    /// Build with an already-resolved credential, skipping the
    /// credential store entirely.
    pub async fn with_credential(
        config: Config,
        credential: ResolvedCredential,
    ) -> Result<Self, FlickError>;
}
```

This eliminates all filesystem dependency for library users who don't want `~/.flick/`.

## What Moves Where

### To library crate (all current `src/` modules except main.rs CLI logic)

| Module | Changes |
|--------|---------|
| `lib.rs` | Expanded: public API surface, `FlickClient` struct |
| `config.rs` | Add `from_str()`. No other changes. |
| `context.rs` | No changes. Already pure data + serialization. |
| `credential.rs` | No changes. Already accepts explicit directory. |
| `error.rs` | No changes. |
| `history.rs` | No changes. Called optionally by CLI. |
| `model.rs` | No changes. |
| `model_list.rs` | No changes. |
| `provider.rs` | Minor: accept `reqwest::Client` in provider constructors instead of creating internally. |
| `provider/messages.rs` | Accept `reqwest::Client` parameter in `new()`. |
| `provider/chat_completions.rs` | Accept `reqwest::Client` parameter in `new()`. |
| `provider/http.rs` | No changes. |
| `result.rs` | No changes. |
| `runner.rs` | No changes. Already takes injected provider + config. |

### To CLI crate (thin wrapper)

| Component | Source |
|-----------|--------|
| Arg parsing (clap) | From current `main.rs` lines 1-72 |
| `cmd_run` / `cmd_run_core` | From current `main.rs` — calls `FlickClient` |
| `cmd_setup` / `cmd_list` / `cmd_init` | From current `main.rs` |
| `TerminalPrompter` | From current `prompter.rs` |
| stdin reading | From current `main.rs` |
| Context file I/O | From current `main.rs` (save/load context files) |
| History recording | From current `main.rs` (calls `history::record`) |

## Migration Strategy

### Phase 1: Restructure into workspace (no API changes) — DONE

1. Create workspace `Cargo.toml` at root.
2. Move `src/` to `flick/src/`.
3. Extract CLI-specific code into `flick-cli/src/main.rs`.
4. `flick-cli` depends on `flick` library.
5. All existing tests pass. Binary name unchanged (`flick`).

### Phase 2: Introduce `FlickClient` — DONE

1. Add `FlickClient` struct to `flick/src/lib.rs`.
2. Provider constructors accept `reqwest::Client` parameter.
3. Add `Config::from_str()`.
4. `FlickClient` takes injected `Box<dyn DynProvider>` — fully testable with mocks.
5. Add `resolve_provider()` standalone function for credential resolution.
6. CLI uses `FlickClient` internally (`cmd_run_core` takes `&FlickClient`).

### Phase 3: Publish

1. Verify public API surface is minimal and stable.
2. Add `#[doc(hidden)]` to internals not meant for external use.
3. Ensure `flick` crate compiles with only library dependencies (no clap).

## Dependency Split

### Library crate (`flick`)

Current runtime dependencies (all stay):
- `reqwest` (HTTP)
- `serde`, `serde_json`, `serde_yaml` (serialization)
- `tokio` (async runtime — but library should not *start* a runtime)
- `chacha20poly1305` (credential encryption)
- `xxhash-rust` (context hashing)

### CLI crate (`flick-cli`)

Additional CLI-only dependencies:
- `clap` (arg parsing)
- `tokio` with `macros` + `rt-multi-thread` features (runtime entry point)

## Design Constraints

1. **Library must not start a tokio runtime.** All async methods assume the caller provides one. The CLI crate owns `#[tokio::main]`.

2. **Library must not write to stdout/stderr.** All output is via return values. The CLI crate handles printing.

3. **Library must not call `std::process::exit`.** Errors are returned, not fatal.

4. **Context persistence is opt-in.** `FlickClient::run()` returns a `FlickResult` containing the updated `Context`. The caller decides whether to persist it. The CLI crate writes context files; library users may keep context in memory.

5. **History recording is opt-in.** The `history` module is public but not called automatically. The CLI crate calls it; library users may skip it.

6. **Backward compatibility.** The `flick` CLI binary behaves identically. Same args, same output, same exit codes.

## What This Does NOT Change

- Config file format (YAML/JSON)
- Output JSON schema
- Credential store format or location (for CLI users)
- Context file format
- Any CLI flags or behavior
- The monadic single-shot invocation model

## Resolved Questions

1. **Workspace vs single crate?** — **Workspace.** Keeps clap and CLI deps out of the library crate entirely. Aligns with the "ultra-small" principle (clap alone adds ~10 transitive crates).

2. **Should `FlickClient` own the `Context`, or should callers pass it in?** — **Caller owns context.** Aligns with the monadic/single-shot principle: each call is a pure function of its inputs. Client stays stateless and `&self`-shareable. A convenience session wrapper can be added later on top.

3. **Should `history` and `credential` modules be public?** — **Public.** Both are part of the stable API from the start. Allows Rust callers to reuse the credential store and history format without reaching into internals.
