# Changelog

All notable changes to the `mech` crate will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
