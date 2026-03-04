# Flick — Architecture

## Data Flow

**New session** (`--query`):
```
CLI args
  → Config::load() + CredentialStore::get()
  → create_provider() → Box<dyn DynProvider>
  → Context (empty) + user query
  → runner::run()  [single model call]
      ├─ config.tools() → Vec<ToolDefinition>
      ├─ provider.call_boxed(params) → ModelResponse
      ├─ append assistant message to context
      └─ return FlickResult (status: complete | tool_calls_pending)
  → write context file, set context_hash
  → serialize FlickResult as JSON to stdout
```

**Resume session** (`--resume <hash>` + `--tool-results <file>`):
```
CLI args
  → Config::load() + CredentialStore::get()
  → create_provider() → Box<dyn DynProvider>
  → Context (loaded from ~/.flick/contexts/{hash}.json)
  → load tool results from --tool-results file
  → append tool results as user message to context
  → runner::run()  [single model call]
  → write context file, set context_hash
  → serialize FlickResult as JSON to stdout
```

## Provider Abstraction

`Provider` trait with two methods:
- `call()` — returns `Result<ModelResponse, ProviderError>` (complete response)
- `build_request()` — returns request body as JSON (for `--dry-run`)

`DynProvider` is the object-safe wrapper (`call_boxed()` adapts the async trait method for object safety). `create_provider()` dispatches by `ApiKind`.

Provider quirks are handled by `CompatFlags` (boolean fields in config), not by subclassing.
