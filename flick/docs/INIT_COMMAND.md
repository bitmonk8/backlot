# Flick — `flick init` Command Specification

## Summary

`flick init` interactively creates a new configuration file. It guides the user
through provider selection, model selection, tool enablement, and optional
settings, then writes a commented TOML file ready for use or further
customization.

## Prerequisites

### Add `dialoguer` Dependency

Add `dialoguer = "0.12"` to `Cargo.toml`. This brings in 2 transitive
crates (`console`, `shell-words`). Use `dialoguer` for all interactive
prompts in both `flick setup` and `flick init`.

#### Widget Mapping

| Prompt type | dialoguer widget |
|-------------|-----------------|
| API key | `Password` (hides input) |
| Select from list | `Select` |
| Text input with default | `Input` with `.default()` |
| Yes/no confirmation | `Confirm` |
| Multi-select (tools) | `MultiSelect` |

#### Testability

The current `cmd_setup_core()` uses injected `BufRead + Write` for testing.
dialoguer widgets write directly to the terminal and cannot use injected
streams.

Replace the injected-streams pattern with a **prompt trait**:

```rust
/// Abstraction over interactive prompts, mockable in tests.
pub trait Prompter {
    /// Display a password prompt (hidden input). Returns the entered string.
    fn password(&self, prompt: &str) -> Result<String, FlickError>;

    /// Display a selection list. Returns the index of the selected item.
    fn select(&self, prompt: &str, items: &[String], default: usize)
        -> Result<usize, FlickError>;

    /// Display a text input with an optional default.
    /// Returns the entered string (or default if empty).
    fn input(&self, prompt: &str, default: Option<&str>)
        -> Result<String, FlickError>;

    /// Display a yes/no confirmation. Returns true for yes.
    fn confirm(&self, prompt: &str, default: bool) -> Result<bool, FlickError>;

    /// Display a multi-select list. Returns indices of selected items.
    fn multi_select(&self, prompt: &str, items: &[String], defaults: &[bool])
        -> Result<Vec<usize>, FlickError>;

    /// Print a message to the user (stderr).
    fn message(&self, msg: &str) -> Result<(), FlickError>;
}
```

Two implementations:

- **`TerminalPrompter`** — wraps `dialoguer` widgets, used in production.
  All prompts render to stderr via `dialoguer`'s `with_prompt()` +
  `interact_on()` targeting stderr.
- **`MockPrompter`** — returns pre-programmed responses, used in tests.

Both `cmd_setup_core()` and `cmd_init_core()` accept `&dyn Prompter`
instead of `BufRead + Write`.

#### `flick setup` Refactor

Rewrite `cmd_setup_core()` to use `Prompter`:

**Before (current):**
```rust
async fn cmd_setup_core(
    provider_name: &str,
    mut input: impl BufRead,
    mut output: impl Write,
    store: &CredentialStore,
) -> Result<(), FlickError>
```

**After:**
```rust
async fn cmd_setup_core(
    provider_name: &str,
    prompter: &dyn Prompter,
    store: &CredentialStore,
) -> Result<(), FlickError>
```

The existing `cmd_setup_core` tests change from passing byte buffers to
passing a `MockPrompter` with pre-programmed answers. Test coverage is
preserved — same scenarios, different mechanism.

### Credential Store → Provider Store

The credential store (`~/.flick/credentials`) changes from a flat string map
to nested TOML tables that include the API type and base URL alongside the
encrypted key. This makes `flick setup` a complete provider-onboarding step.

**Old format:**
```toml
anthropic = "enc3:..."
openai = "enc3:..."
```

**New format:**
```toml
[anthropic]
key = "enc3:..."
api = "messages"
base_url = "https://api.anthropic.com"

[openrouter]
key = "enc3:..."
api = "chat_completions"
base_url = "https://openrouter.ai/api"
```

No migration needed (no existing users).

The `base_url` field is always stored explicitly (never omitted), using the
per-API default when the user accepts the default during setup.

#### Affected `CredentialStore` Methods

| Method | Change |
|--------|--------|
| `set(provider, api_key)` | Becomes `set(provider, api_key, api_kind, base_url)` |
| `get(provider) -> String` | Becomes `get(provider) -> ProviderEntry` |
| `list() -> Vec<String>` | Becomes `list() -> Vec<ProviderInfo>` |

New types:
```rust
pub struct ProviderEntry {
    pub key: String,
    pub api: ApiKind,
    pub base_url: String,
}

pub struct ProviderInfo {
    pub name: String,
    pub api: ApiKind,
    pub base_url: String,
}
```

`ApiKind` already exists in `config.rs` as the deserialized form of
`"messages"` / `"chat_completions"`. It should be moved to a shared location
(or re-exported) so `credential.rs` can use it without depending on config.

### Model Registry Extension

`ModelInfo` in `model.rs` gains two optional fields for token limits:

```rust
pub struct ModelInfo {
    pub id: &'static str,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u32>,
}
```

Updated registry values (from Anthropic/OpenAI documentation):

| Model | context_window | max_output_tokens |
|-------|---------------|-------------------|
| `claude-sonnet-4-20250514` | 200,000 | 64,000 |
| `claude-opus-4-20250514` | 200,000 | 32,000 |
| `claude-haiku-3-5-20241022` | 200,000 | 8,192 |
| `gpt-4o` | 128,000 | 16,384 |
| `gpt-4o-mini` | 128,000 | 16,384 |
| `o3-mini` | 200,000 | 100,000 |
| `deepseek-chat` | 64,000 | 8,192 |
| `deepseek-reasoner` | 64,000 | 8,192 |

These values are used as fallback defaults when the provider's model list
endpoint does not include token limit metadata.

### `flick setup` Changes

`flick setup <provider>` becomes a full provider onboarding flow, capturing
three pieces of information via `dialoguer` widgets:

#### 1. API Key — `Password` widget
```
? API key for 'anthropic': ********
```

Input is hidden. Empty input aborts.

#### 2. API Type — `Select` widget (new)

**Inference logic** (applied before prompting):

| Provider name contains | Inferred API |
|------------------------|--------------|
| `anthropic` | `messages` |
| *(no match)* | ask the user |

**Prompt (when inference fails):**
```
? API type:
> chat_completions (OpenAI-compatible)
  messages (Anthropic)
```

Default: `chat_completions` (most providers use it).

#### 3. Base URL — `Input` widget (new)

Default is inferred from API type:
- `messages` → `https://api.anthropic.com`
- `chat_completions` → `https://api.openai.com`

```
? Base URL [https://api.anthropic.com]:
```

User presses Enter to accept default, or types a custom URL.

All three values are passed to `CredentialStore::set()` and persisted together.

#### Multi-API Proxies (LiteLLM, OpenRouter)

Proxies like LiteLLM and OpenRouter expose both the Messages API
(`/v1/messages`) and Chat Completions API (`/v1/chat/completions`) on the
same base URL with the same API key. Register the proxy twice under
different provider names:

```
$ flick setup litellm-anthropic
  API key: sk-...
  API type: messages
  Base URL: http://proxy:4000

$ flick setup litellm-openai
  API key: sk-...
  API type: chat_completions
  Base URL: http://proxy:4000
```

Each registration gets its own API type. `flick init` then lets the user
pick the right one for their config.

### `flick list` Changes

`flick list` output changes to include API type and base URL:

```
anthropic   messages           https://api.anthropic.com
openrouter  chat_completions   https://openrouter.ai/api
```

Tab-separated, one provider per line. Existing behavior (sorted order) is
preserved.

### Config `max_tokens` Becomes Optional for Chat Completions

Currently `max_tokens` defaults to 8192 and is always sent in API requests.

Change: make it `Option<u32>` in `Config`. Behavior per provider:

- **Messages API**: required by the API. If `None` in config, use the
  registry's `max_output_tokens` for the model, or fall back to 8192.
- **Chat Completions API**: if `None`, omit from the request entirely (the
  API uses the model's own limit).

Validation: if set, must be > 0 (unchanged).

---

## CLI

```
flick init [--output <path>]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--output` | `flick.toml` | Output file path |

If the output file already exists, print a warning and abort (do not overwrite).

## Interactive Flow

All prompts render to **stderr** (via `dialoguer`'s `interact_on()`
targeting stderr). The generated file writes to the output path (or stdout
if `--output -` is given).

### Step 1 — Provider (`Select`)

List onboarded providers (from credential store via `CredentialStore::list()`).

- If one or more providers exist: display a `Select` widget, let user pick.
- If no providers exist: print message suggesting `flick setup <provider>`
  first, then abort.

```
? Select provider:
> anthropic (messages)
  openrouter (chat_completions)
```

Default: first provider in the list.

The API type and base URL are already known from the credential store — no
separate prompts needed.

### Step 2 — Model (`Select` / `Input`)

Query the provider's model list endpoint to discover available models
dynamically.

#### API Call

Using the selected provider's stored base URL and credential:

**Messages API (Anthropic):**
```
GET {base_url}/v1/models
Headers:
  x-api-key: {api_key}
  anthropic-version: 2023-06-01
```

Response: `{"data": [{"id": "claude-sonnet-4-20250514", ...}, ...]}`.
Anthropic paginates with `has_more` + `first_id`/`last_id`; fetch all pages.

**Chat Completions API:**
```
GET {base_url}/v1/models
Headers:
  Authorization: Bearer {api_key}
```

Response: `{"data": [{"id": "gpt-4o", ...}, ...]}`.

Some providers (e.g. OpenRouter) include richer metadata:
- `context_length` — max context window in tokens
- `top_provider.max_completion_tokens` — max output tokens

These fields are extracted when present and used for Step 3 defaults.

#### Display (`Select`)

Extract model IDs from the response, sort alphabetically, append a
"(custom)" entry, and present via `Select`.

```
Fetching models from anthropic...
? Select model:
> claude-haiku-3-5-20241022
  claude-opus-4-20250514
  claude-sonnet-4-20250514
  (custom)
```

If the user selects "(custom)", prompt for the model identifier via `Input`:
```
? Model name:
```

#### Fallback

If the API call fails (network error, auth error, endpoint not supported),
print a warning and fall back to `Input`:

```
Could not fetch models from provider: {error}
? Model name:
```

### Step 3 — Max Tokens (`Input`)

The default is resolved with a three-tier fallback:

1. **Provider model metadata** — if the model list response included
   `max_completion_tokens` or equivalent, use that value.
2. **Built-in registry** — if the selected model ID matches an entry in
   `BUILTIN_MODELS` and it has `max_output_tokens`, use that value.
3. **Hardcoded fallback** — 8192.

For Chat Completions providers, the user may also enter `none` to omit
`max_tokens` entirely (the API will use the model's own default).

```
? Max output tokens [64000]:
```

Or for Chat Completions when no metadata is available:
```
? Max output tokens [8192] (enter 'none' to omit):
```

Accept the default by pressing Enter.

### Step 4 — System Prompt (`Input`)

```
? System prompt [You are Flick, a fast LLM runner.]:
```

Default: `"You are Flick, a fast LLM runner."`. User can accept, replace,
or clear (enter a single space or `none` to omit).

If the value contains newlines or quotes, use a TOML multi-line literal
string (`'''`).

### Step 5 — Builtin Tools (`MultiSelect`)

```
? Enable builtin tools:
  [ ] read_file
  [ ] write_file
  [ ] list_directory
  [ ] shell_exec (unrestricted system access)
```

If `shell_exec` is selected, follow up with a `Confirm`:

```
? shell_exec grants the model unrestricted system access. Enable? [y/N]:
```

### Step 6 — Write File

Show the output path and write the file:
```
Writing config to flick.toml
```

If `--output -`, write to stdout and skip the status message.

## Output Format

The generated TOML file includes section comments explaining each block.
Example output (anthropic provider, claude-sonnet, with read_file enabled):

```toml
# Flick configuration
# Generated by `flick init`. Edit freely.
# Reference: docs/CONFIGURATION.md

# ── System Prompt (optional) ────────────────────────────────────────
system_prompt = "You are Flick, a fast LLM runner."

# ── Model ────────────────────────────────────────────────────────────
# provider: must match a [provider.*] section below
# name:     model identifier
# max_tokens: maximum output tokens (omit to use model default, Chat
#   Completions only; Messages API requires a value)
# temperature: sampling temperature (optional)
#   Messages API: 0.0–1.0 | Chat Completions: 0.0–2.0
[model]
provider = "anthropic"
name = "claude-sonnet-4-20250514"
max_tokens = 64000
# temperature = 0.0

# ── Reasoning (optional) ────────────────────────────────────────────
# level: minimal (1k tokens), low (4k), medium (10k), high (32k)
# For Messages API: budget must be < max_tokens
# [model.reasoning]
# level = "medium"

# ── Provider ─────────────────────────────────────────────────────────
# api: "messages" (Anthropic) or "chat_completions" (OpenAI-compatible)
# base_url: override the default endpoint (optional)
# credential: credential store key (defaults to provider name)
[provider.anthropic]
api = "messages"
# base_url = "https://api.anthropic.com"
# credential = "anthropic"

# ── Compatibility Flags (optional) ──────────────────────────────────
# [provider.anthropic.compat]
# explicit_tool_choice_auto = false

# ── Builtin Tools ───────────────────────────────────────────────────
# shell_exec and custom tools bypass resource restrictions.
[tools]
read_file = true
write_file = false
list_directory = false
shell_exec = false

# ── Custom Tools (optional) ─────────────────────────────────────────
# [[tools.custom]]
# name = "my_tool"
# description = "What the tool does"
# parameters = { type = "object", properties = { arg = { type = "string" } } }
# command = "echo {{arg}}"       # shell command (OR executable, not both)
# executable = "./tools/my_tool" # receives JSON on stdin

# ── Resources (optional) ────────────────────────────────────────────
# Restricts builtin tool access. If omitted, all paths allowed.
# Does NOT restrict shell_exec or custom tools.
# [[resources]]
# path = "src/"
# access = "read_write"

# ── Pricing (optional) ──────────────────────────────────────────────
# Overrides builtin model pricing. Omit to use registry defaults.
# [pricing]
# input_per_million = 3.0
# output_per_million = 15.0

# ── Sandbox (optional) ──────────────────────────────────────────────
# Wrapper prefix for sandboxed tool execution. See docs/SANDBOX.md.
# [sandbox]
# wrapper = ["bwrap", "--die-with-parent"]
# read_args = ["--ro-bind", "{path}", "{path}"]
# read_write_args = ["--bind", "{path}", "{path}"]
# suffix = ["--"]
```

### Generated Provider Section Rules

- If the stored `base_url` matches the default for the API type, emit it as
  a comment (`# base_url = "..."`).
- If the stored `base_url` is non-default, emit it uncommented.
- `credential` is always commented out (defaults to provider name).

### Comment Rules

- Every section gets a header comment with a visual separator line.
- Optional sections that the user did not configure are included as
  commented-out examples.
- Active sections have uncommented key-value pairs.
- Security warnings are included inline where relevant (shell_exec, custom
  tools, resources).

## Errors

| Condition | Behavior |
|-----------|----------|
| Output file exists | Print `"Error: {path} already exists. Use a different --output path."` to stderr, exit 1 |
| No onboarded providers | Print `"No providers configured. Run 'flick setup <provider>' first."` to stderr, exit 1 |
| Empty model name (custom) | Print `"No model name provided, aborting."` to stderr, exit 1 |
| Model fetch fails | Warning to stderr, fall back to `Input` for manual model name entry |
| Stdin closed / EOF | Abort gracefully with message to stderr |
| Write failure | Propagate IO error |

## Testing Strategy

- Both `cmd_setup_core()` and `cmd_init_core()` accept `&dyn Prompter`.
- Unit tests use `MockPrompter` with pre-programmed responses.
- `cmd_init_core()` also accepts a model-fetcher trait for testability.
- Test cases:
  - Single provider auto-selection
  - Multi-provider selection
  - Model list fetched successfully → `Select` selection
  - Model fetch fails → fallback to `Input`
  - Custom model selection
  - Default values accepted
  - Max tokens from provider metadata
  - Max tokens from registry fallback
  - Max tokens hardcoded fallback (8192)
  - Max tokens `none` for Chat Completions
  - System prompt with special characters
  - Tool selection via `MultiSelect`
  - shell_exec `Confirm` rejection
  - shell_exec `Confirm` acceptance
  - Output file exists → error
  - No providers → error
  - `--output -` writes to provided writer
  - Non-default base_url emitted uncommented in output
  - `flick setup` existing tests ported to `MockPrompter`

## Implementation Order

1. **Add `dialoguer` dependency** and create `Prompter` trait + `TerminalPrompter` + `MockPrompter`
2. **Refactor `flick setup`** to use `Prompter`, port existing tests to `MockPrompter`
3. **Move `ApiKind`** to shared location (used by both `config.rs` and `credential.rs`)
4. **Extend model registry** — add `context_window` and `max_output_tokens` to `ModelInfo`
5. **Change credential store format** — update `set`, `get`, `list`, types, and all tests
6. **Make `max_tokens` optional** — `Option<u32>` in config, conditional send in Chat Completions
7. **Update `flick setup`** — add API type + base URL prompts via `Prompter`
8. **Update `flick list`** — show API type and base URL columns
9. **Add model list fetching** — new module with trait for HTTP call + response parsing
10. **Implement `flick init`** — new command with interactive flow

## Scope Exclusions

These are intentionally omitted from v1:

- Editing an existing config file (future: `flick init --edit`)
- Custom tool creation during init
- Resource path configuration during init
- Sandbox configuration during init
- Reasoning level selection (commented-out example is sufficient)
- Output schema configuration
- `--force` flag to overwrite existing files
- Pagination for model list (fetch first page only; sufficient for most providers)
