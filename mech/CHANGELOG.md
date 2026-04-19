# Changelog

All notable changes to the `mech` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `run_function_imperative` now rolls back the failing block's
  `set_context` and `set_workflow` writes before propagating
  `Err(GuardEvaluationError)` (or any other transition-evaluation error,
  or a partial-commit failure). Direct callers of the function (it is
  `pub` and re-exported from `mech::lib`) therefore observe the
  pre-block context and workflow state rather than partial-write side
  effects. Successful guard evaluation still retains the writes — the
  §9.4 retry counter pattern is unaffected. See
  [#463](https://github.com/bitmonk8/backlot/issues/463) and the new
  spec §9.3 rule 7.

### Changed

- **BREAKING:** Workflow validation now rejects blocks named `block`,
  `blocks`, or `meta`. These names are CEL runtime namespaces; a user
  block sharing one silently shadowed the runtime namespace in
  transition guards and templates, producing surprising evaluation
  results.

  **Migration:** Rename any affected blocks before upgrading. Failing
  workflows surface a validation error of the form ``block name `<name>`
  is reserved (conflicts with CEL namespace)``. See
  [#454](https://github.com/bitmonk8/backlot/issues/454).
