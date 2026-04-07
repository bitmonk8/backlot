# Backlot — Dependency Audit

Total unique external crates (including transitive): **~263**

## Summary — Direct External Dependencies

32 direct external dependencies across the workspace (runtime + build + dev).

| # | Crate | Version | Used By | Category | Transitive Deps | License |
|---|-------|---------|---------|----------|-----------------|---------|
| 1 | tokio | 1.51.0 | flick, lot*, reel, reel-cli, vault-cli, epic, cue, lot-cli | async runtime | 12 | MIT |
| 2 | serde | 1.0.228 | flick, lot-cli, reel, reel-cli, vault, vault-cli, epic, cue | serialization | 7 | MIT/Apache-2.0 |
| 3 | serde_json | 1.0.149 | flick, flick-cli, reel-cli, vault, vault-cli, epic | JSON | 5 | MIT/Apache-2.0 |
| 4 | thiserror | 2.0.18 | flick, lot, vault, epic, cue | error types | 6 | MIT/Apache-2.0 |
| 5 | clap | 4.6.0 | flick*, flick-cli, lot-cli, reel-cli, vault-cli, epic | CLI parsing | 21 | MIT/Apache-2.0 |
| 6 | toml | 0.8.23 | flick, epic | TOML parsing | 16 | MIT/Apache-2.0 |
| 7 | serde_yml | 0.0.12 | flick, lot-cli, reel-cli | YAML (serde_yml) | 17 | MIT/Apache-2.0 |
| 8 | serde_yaml | 0.9.34 | vault-cli | YAML (deprecated) | 14 | MIT/Apache-2.0 |
| 9 | anyhow | 1.0.102 | reel, epic, cue | error handling | 1 | MIT/Apache-2.0 |
| 10 | reqwest | 0.12.28 | flick | HTTP client | **105** | MIT/Apache-2.0 |
| 11 | chacha20poly1305 | 0.10.1 | flick | AEAD encryption | 18 | Apache-2.0/MIT |
| 12 | zeroize | 1.8.2 | flick | secret wiping | 6 | Apache-2.0/MIT |
| 13 | hex | 0.4.3 | flick | hex encoding | 1 | MIT/Apache-2.0 |
| 14 | url | 2.5.8 | flick | URL parsing | 32 | MIT/Apache-2.0 |
| 15 | xxhash-rust | 0.8.15 | flick, flick-cli | hashing | 1 | BSL-1.0 |
| 16 | bitflags | 2.11.0 | reel | bitflag types | 1 | MIT/Apache-2.0 |
| 17 | regex | 1.12.3 | vault | regex matching | 5 | MIT/Apache-2.0 |
| 18 | tempfile | 3.27.0 | reel, epic (runtime); flick, lot, lot-cli, vault, vault-cli (dev) | temp files | 7 | MIT/Apache-2.0 |
| 19 | ratatui | 0.29.0 | epic | TUI framework | **46** | MIT |
| 20 | crossterm | 0.28.1 | epic | terminal I/O | 11 | MIT |
| 21 | dialoguer | 0.12.0 | flick-cli | interactive prompts | 14 | MIT |
| 22 | windows | 0.59.0 | flick (Windows only) | Win32 API | 13 | MIT/Apache-2.0 |
| 23 | windows-sys | 0.61.2 | lot (Windows only) | Win32 FFI | 2 | MIT/Apache-2.0 |
| 24 | libc | 0.2.184 | lot (Linux, macOS) | POSIX FFI | 1 | MIT/Apache-2.0 |
| 25 | seccompiler | 0.5.0 | lot (Linux only) | seccomp filters | 0 | Apache-2.0/BSD-3 |
| 26 | ureq | 3.3.0 | reel (build) | HTTP client | 36 | MIT/Apache-2.0 |
| 27 | sha2 | 0.10.9 | reel (build) | SHA-256 | 10 | MIT/Apache-2.0 |
| 28 | flate2 | 1.1.9 | reel (build) | gzip decompression | 6 | MIT/Apache-2.0 |
| 29 | tar | 0.4.45 | reel (build) | tar extraction | 3 | MIT/Apache-2.0 |
| 30 | zip | 2.4.2 | reel (build) | zip extraction | **62** | MIT |
| 31 | wiremock | 0.6.5 | flick (dev) | HTTP mock server | **91** | MIT/Apache-2.0 |
| 32 | serial_test | 3.4.0 | flick (dev) | test serialization | 28 | MIT |

`*` = optional / feature-gated dependency

---

## Dependency Detail

### 1. tokio

| Field | Value |
|-------|-------|
| Version | 1.51.0 |
| License | MIT |
| Repository | https://github.com/tokio-rs/tokio |
| crates.io downloads | ~597M |
| Maintainer | tokio-rs (Carl Lerche, Alice Ryhl, et al.) |
| Transitive deps | 12 |
| Trust | Rust ecosystem foundation. Tokio team is part of the Rust project governance. Heavily audited. |

**Used by:**

| Crate | Features enabled |
|-------|-----------------|
| flick | rt, fs, io-util, time |
| lot | rt, time, macros (optional via `tokio` feature) |
| lot-cli | rt, time, macros |
| reel | rt-multi-thread, macros, time, sync, process, fs, signal, io-util |
| reel-cli | rt, macros |
| vault-cli | rt, macros, rt-multi-thread |
| epic | rt-multi-thread, macros, time, sync, process, fs, signal, io-util |
| cue | sync |

**Feature notes:** The widest feature set is reel/epic (nearly everything). cue only needs `sync` (for mpsc channels). lot's tokio dependency is behind an optional feature gate.

---

### 2. serde

| Field | Value |
|-------|-------|
| Version | 1.0.228 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/serde-rs/serde |
| crates.io downloads | ~910M |
| Maintainer | David Tolnay (dtolnay) |
| Transitive deps | 7 (serde_core, serde_derive, proc-macro2, quote, syn, unicode-ident) |
| Trust | De facto Rust serialization standard. dtolnay is a top Rust contributor. |

**Used by:** flick, lot-cli, reel, reel-cli, vault, vault-cli, epic, cue

**Features enabled:** `default`, `derive` (all consumers use derive)

---

### 3. serde_json

| Field | Value |
|-------|-------|
| Version | 1.0.149 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/serde-rs/json |
| crates.io downloads | ~815M |
| Maintainer | dtolnay |
| Transitive deps | 5 (itoa, ryu, memchr, serde) |
| Trust | Same as serde. |

**Used by:** flick, flick-cli, reel-cli, vault, vault-cli, epic

**Features enabled:** `default`, `std`

---

### 4. thiserror

| Field | Value |
|-------|-------|
| Version | 2.0.18 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/dtolnay/thiserror |
| crates.io downloads | ~874M |
| Maintainer | dtolnay |
| Transitive deps | 6 (proc-macro derive chain) |
| Trust | dtolnay. Standard error derive crate. |

**Used by:** flick, lot, vault, epic, cue

**Features enabled:** `default`, `std`

---

### 5. clap

| Field | Value |
|-------|-------|
| Version | 4.6.0 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/clap-rs/clap |
| crates.io downloads | ~750M |
| Maintainer | Ed Page (epage), clap-rs team |
| Transitive deps | 21 |
| Trust | Standard CLI argument parser for Rust. Widely adopted. |

**Used by:**

| Crate | Features |
|-------|----------|
| flick | `derive` (optional, behind `cli` feature gate) |
| flick-cli | `derive`, `color`, `help`, `suggestions`, `usage`, `error-context` (default) |
| lot-cli | default + `derive` |
| reel-cli | default + `derive` |
| vault-cli | default + `derive` |
| epic | default + `derive` + `env` |

**Feature notes:** epic additionally enables `env` for environment variable fallback. flick's clap is optional — only compiled when the `cli` feature is active.

---

### 6. toml

| Field | Value |
|-------|-------|
| Version | 0.8.23 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/toml-rs/toml |
| crates.io downloads | ~559M |
| Maintainer | toml-rs team (Ed Page) |
| Transitive deps | 16 (toml_edit, toml_datetime, toml_write, winnow, serde_spanned, indexmap, hashbrown, equivalent) |
| Trust | Standard TOML parser for Rust. |

**Used by:** flick, epic

**Features enabled:** `default` (display, parse)

---

### 7. serde_yml

| Field | Value |
|-------|-------|
| Version | 0.0.12 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/sebastienrousseau/serde_yml |
| crates.io downloads | ~12M |
| Maintainer | Sebastien Rousseau |
| Transitive deps | 17 (libyml, indexmap, etc.) |
| Trust | **Low adoption** relative to other serde ecosystem crates. Single maintainer. Successor to the deprecated `serde_yaml` but from a different author. |

**Used by:** flick, lot-cli, reel-cli (workspace dep)

**Features enabled:** `default`

---

### 8. serde_yaml (DEPRECATED)

| Field | Value |
|-------|-------|
| Version | 0.9.34+deprecated |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/dtolnay/serde-yaml |
| crates.io downloads | ~249M |
| Maintainer | dtolnay (deprecated) |
| Transitive deps | 14 (unsafe-libyaml, indexmap, etc.) |
| Trust | Well-established but **explicitly deprecated** by the author. No further updates. |

**Used by:** vault-cli only

**Notes:** vault-cli uses `serde_yaml` while the rest of the workspace uses `serde_yml`. This means the workspace ships **two** YAML libraries simultaneously. Their transitive dep trees overlap heavily (indexmap, serde, etc.) but each brings its own YAML C binding (libyml vs unsafe-libyaml).

---

### 9. anyhow

| Field | Value |
|-------|-------|
| Version | 1.0.102 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/dtolnay/anyhow |
| crates.io downloads | ~613M |
| Maintainer | dtolnay |
| Transitive deps | 1 (itself) |
| Trust | dtolnay. Zero-dep error boxing. |

**Used by:** reel, epic, cue

**Features enabled:** `default`, `std`

---

### 10. reqwest

| Field | Value |
|-------|-------|
| Version | 0.12.28 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/seanmonstar/reqwest |
| crates.io downloads | ~425M |
| Maintainer | Sean McArthur (seanmonstar) |
| Transitive deps | **105** |
| Trust | De facto async HTTP client for Rust. hyper author. |

**Used by:** flick

**Features enabled:** `rustls-tls` (no OpenSSL), `json`; `default-features = false`

**Feature notes:** `default-features = false` disables the default TLS backend. `rustls-tls` pulls in rustls, ring, webpki-roots. This is the **heaviest** runtime dependency in the workspace.

Transitive dep chain includes: hyper, h2, http, http-body, http-body-util, hyper-util, hyper-rustls, tokio-rustls, rustls, ring, webpki-roots, tower, tower-service, tower-layer, tower-http, futures, futures-core/channel/util, bytes, base64, form_urlencoded, percent-encoding, url, idna (with full ICU4X: icu_normalizer, icu_properties, icu_collections, icu_provider, etc.), ipnet, serde_urlencoded, sync_wrapper, pin-project-lite, tracing, tracing-core, and more.

---

### 11. chacha20poly1305

| Field | Value |
|-------|-------|
| Version | 0.10.1 |
| License | Apache-2.0 OR MIT |
| Repository | https://github.com/RustCrypto/AEADs |
| crates.io downloads | ~51M |
| Maintainer | RustCrypto team |
| Transitive deps | 18 |
| Trust | RustCrypto org. Well-reviewed cryptographic implementations. |

**Used by:** flick

**Features enabled:** `alloc`, `default`, `getrandom`, `rand_core`

Transitive chain: chacha20, cipher, poly1305, universal-hash, aead, aes (for AEAD), inout, generic-array, typenum, subtle, opaque-debug, crypto-common, block-buffer, cpufeatures, zeroize, getrandom, rand_core, cfg-if.

---

### 12. zeroize

| Field | Value |
|-------|-------|
| Version | 1.8.2 |
| License | Apache-2.0 OR MIT |
| Repository | https://github.com/RustCrypto/utils |
| crates.io downloads | ~404M |
| Maintainer | RustCrypto team |
| Transitive deps | 6 |
| Trust | RustCrypto. Security-critical secret wiping. |

**Used by:** flick

**Features enabled:** `alloc`, `default`

---

### 13. hex

| Field | Value |
|-------|-------|
| Version | 0.4.3 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/KokaKiwi/rust-hex |
| crates.io downloads | ~414M |
| Maintainer | KokaKiwi |
| Transitive deps | 1 (itself) |
| Trust | Simple, well-adopted, minimal. |

**Used by:** flick

**Features enabled:** `alloc`, `default`, `std`

---

### 14. url

| Field | Value |
|-------|-------|
| Version | 2.5.8 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/servo/rust-url |
| crates.io downloads | ~562M |
| Maintainer | Servo project |
| Transitive deps | **32** |
| Trust | Servo/Mozilla lineage. WHATWG URL standard implementation. |

**Used by:** flick

**Features enabled:** `default`, `std`

**Notes:** The high transitive count comes from IDNA handling, which pulls in ICU4X crates (icu_normalizer, icu_properties, icu_collections, etc.). These are also pulled by reqwest, so they overlap.

---

### 15. xxhash-rust

| Field | Value |
|-------|-------|
| Version | 0.8.15 |
| License | BSL-1.0 (Boost Software License) |
| Repository | https://github.com/DoumanAsh/xxhash-rust |
| crates.io downloads | ~57M |
| Maintainer | DoumanAsh |
| Transitive deps | 1 (itself) |
| Trust | Single maintainer. BSL-1.0 is permissive but non-standard in the Rust ecosystem. |

**Used by:** flick, flick-cli

**Features enabled:** `xxh3`

---

### 16. bitflags

| Field | Value |
|-------|-------|
| Version | 2.11.0 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/bitflags/bitflags |
| crates.io downloads | ~1.17B |
| Maintainer | Rust ecosystem team (Alex Crichton, et al.) |
| Transitive deps | 1 (itself) |
| Trust | Rust standard library adjacent. Most downloaded crate on crates.io. |

**Used by:** reel

**Features enabled:** none (no default features; just the core macro)

---

### 17. regex

| Field | Value |
|-------|-------|
| Version | 1.12.3 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/rust-lang/regex |
| crates.io downloads | ~756M |
| Maintainer | Andrew Gallant (BurntSushi), rust-lang |
| Transitive deps | 5 (regex-automata, regex-syntax, aho-corasick, memchr) |
| Trust | rust-lang org. Foundation crate. |

**Used by:** vault

**Features enabled:** `default` (full Unicode support, all performance optimizations: perf-backtrack, perf-cache, perf-dfa, perf-inline, perf-literal, perf-onepass)

**Notes:** Default features enable all Unicode tables and all optimization engines. If vault only uses simple ASCII patterns, `default-features = false` + `std` + `perf` would reduce compile-time code.

---

### 18. tempfile

| Field | Value |
|-------|-------|
| Version | 3.27.0 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/Stebalien/tempfile |
| crates.io downloads | ~512M |
| Maintainer | Steven Allen (Stebalien) |
| Transitive deps | 7 |
| Trust | Widely adopted. Simple scope. |

**Used by:**
- **Runtime:** reel, epic
- **Dev only:** flick, lot, lot-cli, vault, vault-cli

**Features enabled:** `default`, `getrandom`

---

### 19. ratatui

| Field | Value |
|-------|-------|
| Version | 0.29.0 |
| License | MIT |
| Repository | https://github.com/ratatui/ratatui |
| crates.io downloads | ~23M |
| Maintainer | ratatui team (Josh McKinney, et al.) |
| Transitive deps | **46** |
| Trust | Active community. Successor to tui-rs. |

**Used by:** epic

**Features enabled:** `crossterm` (default), `default`, `underline-color`

Transitive chain includes: crossterm, compact_str, castaway, cassowary, unicode-segmentation, unicode-truncate, unicode-width (0.1 + 0.2), itertools, either, instability, strum, strum_macros, indoc, paste, lru, hashbrown.

---

### 20. crossterm

| Field | Value |
|-------|-------|
| Version | 0.28.1 |
| License | MIT |
| Repository | https://github.com/crossterm-rs/crossterm |
| crates.io downloads | ~116M |
| Maintainer | crossterm-rs team |
| Transitive deps | 11 |
| Trust | Standard cross-platform terminal library. |

**Used by:** epic (also pulled transitively by ratatui)

**Features enabled:** `bracketed-paste`, `default`, `events`, `windows`

Platform deps: `crossterm_winapi` (Windows), `mio`, `parking_lot`, `signal-hook`/`signal-hook-mio` (Unix).

---

### 21. dialoguer

| Field | Value |
|-------|-------|
| Version | 0.12.0 |
| License | MIT |
| Repository | https://github.com/console-rs/dialoguer |
| crates.io downloads | ~53M |
| Maintainer | console-rs team (Armin Ronacher / mitsuhiko) |
| Transitive deps | 14 |
| Trust | Armin Ronacher (Flask author). Well-maintained. |

**Used by:** flick-cli

**Features enabled:** `default` (editor, password, tempfile, zeroize)

---

### 22. windows

| Field | Value |
|-------|-------|
| Version | 0.59.0 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/microsoft/windows-rs |
| crates.io downloads | ~206M |
| Maintainer | Microsoft |
| Transitive deps | 13 |
| Trust | Official Microsoft crate. |

**Used by:** flick (Windows only)

**Features enabled:**
- `Win32_Foundation`
- `Win32_Security`
- `Win32_Security_Authorization`
- `Win32_System_Threading`

**Notes:** This is the high-level `windows` crate (with `windows-core`, `windows-strings`, etc.), not the thin FFI `windows-sys`. flick uses it for DPAPI-based key encryption on Windows.

---

### 23. windows-sys

| Field | Value |
|-------|-------|
| Version | 0.61.2 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/microsoft/windows-rs |
| crates.io downloads | ~848M |
| Maintainer | Microsoft |
| Transitive deps | 2 (windows-link, windows-targets) |
| Trust | Official Microsoft crate. Zero-overhead FFI bindings. |

**Used by:** lot (Windows only)

**Features enabled:**
- `Win32_Foundation`
- `Win32_Security`, `Win32_Security_Authorization`, `Win32_Security_Isolation`
- `Win32_Storage_FileSystem`
- `Win32_System_Console`, `Win32_System_JobObjects`, `Win32_System_Performance`, `Win32_System_Pipes`, `Win32_System_SystemInformation`, `Win32_System_Threading`

**Notes:** `windows-sys` is the thin FFI-only crate (no COM wrappers). lot also depends on `windows-sys` transitively via tokio on Windows.

---

### 24. libc

| Field | Value |
|-------|-------|
| Version | 0.2.184 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/rust-lang/libc |
| crates.io downloads | ~1.04B |
| Maintainer | rust-lang |
| Transitive deps | 1 (itself) |
| Trust | rust-lang org. Second most downloaded crate. |

**Used by:** lot (Linux + macOS, for namespace/seatbelt syscalls)

---

### 25. seccompiler

| Field | Value |
|-------|-------|
| Version | 0.5.0 |
| License | Apache-2.0 OR BSD-3-Clause |
| Repository | https://github.com/rust-vmm/seccompiler |
| crates.io downloads | ~11M |
| Maintainer | rust-vmm (Firecracker / AWS) |
| Transitive deps | 0 (depends on libc, already counted) |
| Trust | AWS/Firecracker project. Production-grade seccomp filter compiler. |

**Used by:** lot (Linux only)

---

### 26. ureq (build dependency)

| Field | Value |
|-------|-------|
| Version | 3.3.0 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/algesten/ureq |
| crates.io downloads | ~110M |
| Maintainer | Martin Algesten |
| Transitive deps | 36 |
| Trust | Widely used synchronous HTTP client. Pulls in rustls/ring for HTTPS. |

**Used by:** reel (build.rs — downloads NuShell and ripgrep binaries)

**Features enabled:** `default` (ring, rustls, gzip)

---

### 27. sha2 (build dependency)

| Field | Value |
|-------|-------|
| Version | 0.10.9 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/RustCrypto/hashes |
| crates.io downloads | ~541M |
| Maintainer | RustCrypto team |
| Transitive deps | 10 |
| Trust | RustCrypto. Standard SHA-2 implementation. |

**Used by:** reel (build.rs — checksum verification)

**Features enabled:** `default`, `std`

---

### 28. flate2 (build dependency)

| Field | Value |
|-------|-------|
| Version | 1.1.9 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/rust-lang/flate2-rs |
| crates.io downloads | ~433M |
| Maintainer | rust-lang (Alex Crichton) |
| Transitive deps | 6 |
| Trust | rust-lang org. Pure-Rust miniz_oxide backend by default. |

**Used by:** reel (build.rs — .tar.gz extraction)

**Features enabled:** `default` (miniz_oxide/rust_backend)

---

### 29. tar (build dependency)

| Field | Value |
|-------|-------|
| Version | 0.4.45 |
| License | MIT OR Apache-2.0 |
| Repository | https://github.com/alexcrichton/tar-rs |
| crates.io downloads | ~146M |
| Maintainer | Alex Crichton |
| Transitive deps | 3 (filetime, libc/windows-sys) |
| Trust | Alex Crichton (Rust team). |

**Used by:** reel (build.rs — .tar.gz extraction)

**Features enabled:** `default` (xattr)

---

### 30. zip (build dependency)

| Field | Value |
|-------|-------|
| Version | 2.4.2 |
| License | MIT |
| Repository | https://github.com/zip-rs/zip2 |
| crates.io downloads | ~158M |
| Maintainer | zip-rs team |
| Transitive deps | **62** |
| Trust | Community maintained. zip2 is the maintained fork of the original zip crate. |

**Used by:** reel (build.rs — .zip extraction on Windows)

**Features enabled:** `default` — this enables **everything**: `aes-crypto`, `bzip2`, `deflate-flate2`, `deflate-zopfli`, `deflate64`, `lzma`, `time`, `xz`, `zstd`

**Notes:** This is the second-heaviest dependency in the workspace. The default feature set pulls in: aes, bzip2/bzip2-sys (C code!), constant_time_eq, crc, crc32fast, deflate64, flate2, hmac, lzma-rs/lzma-sys (C code!), pbkdf2, sha1, time, xz2 (C code!), zeroize, zopfli, zstd/zstd-sys (C code!). reel only needs to extract .zip files containing a single binary — `default-features = false` + `deflate-flate2` would eliminate ~40 transitive deps and all C compilation.

---

### 31. wiremock (dev dependency)

| Field | Value |
|-------|-------|
| Version | 0.6.5 |
| License | MIT/Apache-2.0 |
| Repository | https://github.com/LukeMathWalker/wiremock-rs |
| crates.io downloads | ~47M |
| Maintainer | Luca Palmieri (LukeMathWalker) |
| Transitive deps | **91** |
| Trust | "Zero to Production in Rust" author. Well-maintained. |

**Used by:** flick (dev only — mock HTTP server for provider tests)

**Notes:** Dev-only. Does not affect release binary size. Pulls in full hyper/tokio/http stack plus deadpool, futures, etc.

---

### 32. serial_test (dev dependency)

| Field | Value |
|-------|-------|
| Version | 3.4.0 |
| License | MIT |
| Repository | https://github.com/palfrey/serial_test |
| crates.io downloads | ~103M |
| Maintainer | Tom Parker-Shemilt (palfrey) |
| Transitive deps | 28 |
| Trust | Widely used. |

**Used by:** flick (dev only — serializes tests that share filesystem state)

**Features enabled:** `async`, `default`, `logging`

---

## Observations

### Duplicate YAML libraries

The workspace ships **two** YAML parsers:
- `serde_yml` 0.0.12 — used by flick, lot-cli, reel-cli (workspace dep)
- `serde_yaml` 0.9.34+deprecated — used by vault-cli only

Each pulls its own C YAML binding (libyml vs unsafe-libyaml). Consolidating vault-cli to `serde_yml` would remove `serde_yaml` + `unsafe-libyaml` entirely.

### Duplicate Win32 API crates

flick uses `windows` 0.59 (high-level, COM-capable) while lot uses `windows-sys` 0.61 (thin FFI). These serve different purposes — `windows` provides safe wrappers for DPAPI calls, `windows-sys` provides raw FFI for AppContainer/Job Object calls. Both are from Microsoft. The `windows` crate is significantly heavier (pulls `windows-core`, `windows-strings`, proc macros). Could potentially migrate flick to `windows-sys` if the DPAPI calls can be done via raw FFI, saving ~11 transitive deps.

### zip default features are overkill

`zip` 2.4.2 with default features pulls **62 transitive deps** including 4 C libraries (bzip2-sys, lzma-sys, xz2, zstd-sys). reel's build.rs only extracts a single binary from NuShell/ripgrep .zip releases — these are standard deflate-compressed zips. Using `default-features = false, features = ["deflate-flate2"]` would eliminate ~40 deps and all C compilation in the build script.

### reqwest is the heaviest runtime dep

At **105 transitive deps**, reqwest dwarfs everything else. This is partially unavoidable for a full-featured async HTTP client with TLS. The `default-features = false` + `rustls-tls` + `json` configuration is already minimal. Alternatives:
- `ureq` (already used in build.rs) is synchronous — not suitable for flick's async architecture
- `hyper` directly would be lower-level but still carry most of the same deps (it's reqwest's foundation)

### url's IDNA/ICU4X weight

`url` 2.5.8 pulls **32 transitive deps**, mostly from the ICU4X-based IDNA implementation (icu_normalizer, icu_properties, icu_collections, etc.). These overlap with reqwest's deps. If url were removed, these would still be present via reqwest. However, if reqwest were ever replaced, url's IDNA deps would become a standalone cost.

### serde_yml trust concern

`serde_yml` has ~12M downloads vs `serde_yaml`'s ~249M. It's maintained by a single developer and is at version 0.0.12 (pre-1.0). The Rust ecosystem hasn't converged on a clear successor to dtolnay's deprecated `serde_yaml`. Alternative: use `toml` for configuration (already in the workspace) and remove YAML entirely where possible.

### Build dependency weight

reel's build dependencies (ureq + sha2 + flate2 + tar + zip) total ~100+ transitive deps. These only run at build time and don't affect the release binary. However, they increase:
- Clean build time
- `cargo update` audit surface
- Supply chain attack surface during CI

### Feature gate opportunities

| Crate | Current features | Potential reduction |
|-------|-----------------|---------------------|
| zip | all (default) | `deflate-flate2` only — saves ~40 deps |
| regex | all Unicode + all perf | `std` + `perf` if ASCII-only — saves Unicode tables |
| tokio (cue) | `sync` only | Already minimal |
| tokio (flick) | `rt, fs, io-util, time` | Already minimal for async HTTP client usage |
| ratatui | `default` (crossterm) | Already minimal for TUI |
| dialoguer | `default` (editor, password) | `password` only if editor not used |
| ureq | `default` (ring, rustls, gzip) | `rustls` + `gzip` without ring if native-tls acceptable — marginal gain |

### Crates with non-standard licenses

| Crate | License | Notes |
|-------|---------|-------|
| xxhash-rust | BSL-1.0 | Boost Software License. Permissive, OSI-approved, but unusual in Rust ecosystem |
| seccompiler | Apache-2.0 OR BSD-3-Clause | Standard permissive. No concern. |

All other crates are MIT or MIT/Apache-2.0 dual-licensed.

---

## Action Plan — Dependency Reduction

#### 1. zip: disable default features

**Estimated savings:** ~40 transitive deps, eliminate 4 C library compilations (bzip2-sys, lzma-sys, xz2, zstd-sys)

reel's build.rs extracts a single binary from deflate-compressed `.zip` files (NuShell and ripgrep GitHub releases). The default feature set enables every compression algorithm and AES encryption.

Change in `reel/reel/Cargo.toml`:
```toml
# Before
zip = "2"

# After
zip = { version = "2", default-features = false, features = ["deflate-flate2"] }
```

`deflate-flate2` enables deflate decompression via flate2 (already a build dep). If NuShell/ripgrep zips use the `stored` method for some entries, `deflate-flate2` still handles that — stored entries require no feature flag.

**Risk:** If a future NuShell release ships a zip with bzip2 or zstd compression, extraction would fail at build time with a clear error. This is unlikely — GitHub release zips use standard deflate.

**Verification:** `cargo build -p reel` on all three platforms (the build.rs downloads and extracts binaries).

---

#### 2. Replace dialoguer with rpassword

**Estimated savings:** ~12 transitive deps (dialoguer + console + shell-words + encode_unicode + tempfile + zeroize, vs rpassword's ~2 deps)

`dialoguer` 0.12 with default features pulls 14 transitive deps. flick-cli uses it solely for password-style API key input (hidden echo). `rpassword` (v7.4, ~40M downloads, https://github.com/conradkleinespel/rpassword) does exactly this with ~2 transitive deps (libc on Unix, winapi on Windows — both already in the dep tree).

Changes:
1. `flick/flick-cli/Cargo.toml`: replace `dialoguer = "0.12"` with `rpassword = "7"`
2. Replace `dialoguer::Password::new().with_prompt("API key").interact()` with `rpassword::prompt_password("API key: ")`

**Risk:** Minimal. rpassword is a focused, well-adopted crate. No interactive confirmation or retry logic — flick-cli doesn't use those dialoguer features.

---

#### 3. Eliminate serde_yaml, consolidate on serde_yml

**Estimated savings:** ~2 transitive deps (unsafe-libyaml, serde_yaml itself); eliminates a deprecated crate

vault-cli is the sole consumer of `serde_yaml`. All other YAML usage goes through `serde_yml`.

Changes:
1. `vault/vault-cli/Cargo.toml`: replace `serde_yaml = "0.9"` with `serde_yml.workspace = true`
2. `vault/vault-cli/src/`: replace `serde_yaml::from_str` → `serde_yml::from_str` (API-compatible for basic deserialization)

**Risk:** `serde_yml` has a different YAML C binding (libyml vs unsafe-libyaml). Behavior should be identical for standard YAML config files. Test vault-cli config loading.

---

#### 4. regex: disable Unicode features

**Estimated savings:** Reduced compile-time codegen (Unicode tables). Transitive dep count stays at 5 but binary size and compile time decrease.

vault uses regex for document name validation and content pattern matching. If all patterns are ASCII (no Unicode character class matching like `\p{Letter}`), the full Unicode feature set is waste.

**Investigation required:** Grep vault source for regex patterns to confirm ASCII-only usage. If confirmed:

```toml
# Before
regex = "1"

# After
regex = { version = "1", default-features = false, features = ["std", "perf"] }
```

This keeps all performance optimizations but drops Unicode tables (~500KB of compiled data).

**Risk:** If any regex pattern uses Unicode classes, matching will fail. Compile-time error if `unicode` feature is referenced in code.

---

#### 5. Migrate flick from windows to windows-sys (Windows-only)

**Estimated savings:** ~11 transitive deps (windows-core, windows-strings, windows-implement, windows-interface, and their proc-macro chains)

flick uses the high-level `windows` crate for DPAPI key encryption (`CryptProtectData` / `CryptUnprotectData`). These are plain Win32 functions that don't require COM wrappers or safe string types. Raw FFI via `windows-sys` suffices.

Changes:
1. `flick/flick/Cargo.toml`: replace `windows` with `windows-sys`, add required feature flags (`Win32_Security_Cryptography` for DPAPI)
2. Rewrite DPAPI calls to use raw FFI (unsafe blocks with `// SAFETY:` comments)
3. lot already depends on `windows-sys` 0.61, so this eliminates a separate Microsoft crate

**Risk:** Moderate. Raw FFI requires careful unsafe code. The safe wrappers in `windows` handle memory management (DATA_BLOB lifetime, LocalFree). Raw FFI needs manual memory management. Thorough testing required.
