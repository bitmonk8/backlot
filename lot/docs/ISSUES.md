# Known Issues

Issues grouped by co-fixability, ordered by descending impact.

---

## Group 12: macOS Seatbelt Test Precision

Non-UTF-8 path handling tests need broader coverage and stricter assertions.

| # | File | Lines | Description | Severity |
|---|------|-------|-------------|----------|
| 42 | lot/src/macos/seatbelt.rs | tests | Non-UTF-8 error tests (`generate_profile_errors_on_non_utf8_read_path`, `_deny_path`) only check `msg.contains("not valid UTF-8")`. Should verify error variant and which path triggered the error. | Medium |
| 43 | lot/src/macos/seatbelt.rs | tests | Missing non-UTF-8 test coverage for write_paths and exec_paths vectors. Only read and deny paths are tested. | Medium |
| 44 | lot/src/macos/seatbelt.rs | tests | The two non-UTF-8 test functions share nearly identical bodies. Could be consolidated with a parameterized helper. | Low |
