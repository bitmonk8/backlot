// Storage primitives consumed by the operations layer. Some methods are only
// used by tests or future operations; allow dead_code until all operations
// are implemented.
#![allow(dead_code)]

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("invalid raw document name: {0}")]
    InvalidName(String),

    #[error("version conflict for document '{0}': versions already exist")]
    VersionConflict(String),

    #[error("document not found: '{0}'")]
    DocumentNotFound(String),
}

// ---------------------------------------------------------------------------
// Changelog types
// ---------------------------------------------------------------------------

/// A single entry in the JSONL changelog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op")]
pub enum ChangelogEntry {
    #[serde(rename = "bootstrap")]
    Bootstrap { ts: String, raw: String },
    #[serde(rename = "record")]
    Record {
        ts: String,
        raw: String,
        derived_modified: Vec<String>,
    },
    #[serde(rename = "reorganize")]
    Reorganize {
        ts: String,
        merged: Vec<String>,
        restructured: Vec<String>,
        deleted: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Document inventory types
// ---------------------------------------------------------------------------

/// A reference to a document (filename only, no directory prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentRef {
    pub filename: String,
}

/// A raw document with its parsed base name and version number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDocumentVersion {
    pub base_name: String,
    pub version: u32,
    pub filename: String,
}

/// Result of validating a single derived document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedValidationWarning {
    pub filename: String,
    pub reason: String,
}

/// Full inventory of the storage root.
#[derive(Debug, Clone, Default)]
pub struct DocumentInventory {
    pub raw: Vec<RawDocumentVersion>,
    pub derived: Vec<DocumentRef>,
}

// ---------------------------------------------------------------------------
// Name validation
// ---------------------------------------------------------------------------

static RAW_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z][A-Z0-9_]*[A-Z0-9]$").unwrap_or_else(|e| panic!("invalid regex: {e}"))
});

static DERIVED_FILENAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z][A-Z0-9_]*[A-Z0-9]\.md$").unwrap_or_else(|e| panic!("invalid regex: {e}"))
});

static RAW_VERSIONED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Z][A-Z0-9_]*[A-Z0-9])_(\d+)\.md$")
        .unwrap_or_else(|e| panic!("invalid regex: {e}"))
});

/// Validate a raw document base name (without version suffix).
/// Returns true if the name matches `^[A-Z][A-Z0-9_]*[A-Z0-9]$` (min 2 chars).
pub fn is_valid_raw_name(name: &str) -> bool {
    name.len() >= 2 && RAW_NAME_RE.is_match(name)
}

/// Check if a name looks like it already has a version suffix (e.g., "FINDINGS_2").
/// This is used to reject names that end with `_\d+`.
pub fn has_version_suffix(name: &str) -> bool {
    if let Some(idx) = name.rfind('_') {
        let suffix = &name[idx + 1..];
        !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

/// Manages the on-disk storage root for a vault instance.
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    /// Create a new `Storage` handle. Does not create or validate the directory.
    pub const fn new(storage_root: PathBuf) -> Self {
        Self { root: storage_root }
    }

    // -- paths --

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn changelog_path(&self) -> PathBuf {
        self.root.join("CHANGELOG.md")
    }

    pub fn raw_dir(&self) -> PathBuf {
        self.root.join("raw")
    }

    pub fn derived_dir(&self) -> PathBuf {
        self.root.join("derived")
    }

    // -- existence checks --

    /// Returns `true` if any of CHANGELOG.md, raw/, or derived/ already exist.
    pub fn is_initialized(&self) -> bool {
        self.changelog_path().exists() || self.raw_dir().exists() || self.derived_dir().exists()
    }

    // -- directory creation --

    /// Create the `raw/` and `derived/` directories. Does NOT create the
    /// storage root itself (that must already exist).
    pub fn create_directories(&self) -> Result<(), StorageError> {
        fs::create_dir_all(self.raw_dir())?;
        fs::create_dir_all(self.derived_dir())?;
        Ok(())
    }

    // -- changelog --

    /// Append a changelog entry as a single JSONL line.
    pub fn append_changelog(&self, entry: &ChangelogEntry) -> Result<(), StorageError> {
        let line = serde_json::to_string(entry).map_err(io::Error::other)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.changelog_path())?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Read all changelog entries.
    pub fn read_changelog(&self) -> Result<Vec<ChangelogEntry>, StorageError> {
        let path = self.changelog_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)?;
        let mut entries = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: ChangelogEntry = serde_json::from_str(trimmed)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    // -- raw documents --

    /// Scan `raw/` for all versioned files matching a given base name.
    /// Returns versions sorted ascending.
    pub fn scan_versions(&self, base_name: &str) -> Result<Vec<RawDocumentVersion>, StorageError> {
        let mut versions: Vec<RawDocumentVersion> = self
            .list_all_raw()?
            .into_iter()
            .filter(|v| v.base_name == base_name)
            .collect();
        versions.sort_by_key(|v| v.version);
        Ok(versions)
    }

    /// Write a raw document at the given version. Returns the filename.
    fn write_raw(
        &self,
        base_name: &str,
        version: u32,
        content: &str,
    ) -> Result<String, StorageError> {
        let filename = format!("{base_name}_{version}.md");
        let path = self.raw_dir().join(&filename);
        fs::write(&path, content)?;
        Ok(filename)
    }

    /// Read a raw document by filename.
    pub(crate) fn read_raw(&self, filename: &str) -> Result<String, StorageError> {
        let path = self.raw_dir().join(filename);
        if !path.exists() {
            return Err(StorageError::DocumentNotFound(filename.to_owned()));
        }
        Ok(fs::read_to_string(&path)?)
    }

    /// Write a new raw document, handling name validation and version assignment.
    /// If `new_series` is true, creates version 1 (fails if versions exist).
    /// If `new_series` is false, appends the next version (fails if none exist).
    pub fn write_raw_versioned(
        &self,
        base_name: &str,
        content: &str,
        new_series: bool,
    ) -> Result<String, StorageError> {
        if !is_valid_raw_name(base_name) || has_version_suffix(base_name) {
            return Err(StorageError::InvalidName(base_name.to_owned()));
        }

        let versions = self.scan_versions(base_name)?;

        if new_series {
            if !versions.is_empty() {
                return Err(StorageError::VersionConflict(base_name.to_owned()));
            }
            self.write_raw(base_name, 1, content)
        } else {
            if versions.is_empty() {
                return Err(StorageError::DocumentNotFound(base_name.to_owned()));
            }
            let next = versions.last().map_or(1, |v| v.version + 1);
            self.write_raw(base_name, next, content)
        }
    }

    // -- derived documents --

    /// List all files in `derived/`.
    pub fn list_derived(&self) -> Result<Vec<DocumentRef>, StorageError> {
        let dir = self.derived_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut docs = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let fname = entry.file_name().to_string_lossy().into_owned();
                docs.push(DocumentRef { filename: fname });
            }
        }
        docs.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(docs)
    }

    /// Validate all derived documents. Returns warnings (not errors).
    pub fn validate_derived(&self) -> Result<Vec<DerivedValidationWarning>, StorageError> {
        let dir = self.derived_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut warnings = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let fname = entry.file_name().to_string_lossy().into_owned();

            // Filename check
            if !DERIVED_FILENAME_RE.is_match(&fname) {
                warnings.push(DerivedValidationWarning {
                    filename: fname.clone(),
                    reason: "filename does not match UPPERCASE_DESCRIPTIVE.md pattern".to_owned(),
                });
                continue;
            }

            // Header check
            let content = match fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(DerivedValidationWarning {
                        filename: fname,
                        reason: format!("unable to read file: {e}"),
                    });
                    continue;
                }
            };
            let mut lines = content.lines();
            let has_title = lines.next().is_some_and(|l| l.starts_with("# "));
            let has_scope = lines.next().is_some_and(|l| l.starts_with("<!-- scope: "));

            if !has_title {
                warnings.push(DerivedValidationWarning {
                    filename: fname.clone(),
                    reason: "missing title line starting with '# '".to_owned(),
                });
            }
            if !has_scope {
                warnings.push(DerivedValidationWarning {
                    filename: fname,
                    reason: "missing scope comment on second line starting with '<!-- scope: '"
                        .to_owned(),
                });
            }
        }
        warnings.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(warnings)
    }

    // -- inventory --

    /// Build a full document inventory (raw + derived).
    pub fn inventory(&self) -> Result<DocumentInventory, StorageError> {
        let raw = self.list_all_raw()?;
        let derived = self.list_derived()?;
        Ok(DocumentInventory { raw, derived })
    }

    /// List all raw documents across all base names.
    fn list_all_raw(&self) -> Result<Vec<RawDocumentVersion>, StorageError> {
        let dir = self.raw_dir();
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut versions = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy();
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(caps) = RAW_VERSIONED_RE.captures(&fname_str) {
                if let Ok(ver) = caps[2].parse::<u32>() {
                    versions.push(RawDocumentVersion {
                        base_name: caps[1].to_owned(),
                        version: ver,
                        filename: fname_str.into_owned(),
                    });
                }
            }
        }
        versions.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(versions)
    }
}

/// Return the current UTC timestamp in ISO 8601 format.
pub fn utc_now_iso8601() -> String {
    let now = std::time::SystemTime::now();
    let dur = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();

    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let (year, month, day) = days_to_civil(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
/// Algorithm from Howard Hinnant's civil_from_days.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)]
const fn days_to_civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- name validation --

    #[test]
    fn valid_raw_names() {
        assert!(is_valid_raw_name("AB"));
        assert!(is_valid_raw_name("FINDINGS"));
        assert!(is_valid_raw_name("API_DESIGN"));
        assert!(is_valid_raw_name("A1"));
        assert!(is_valid_raw_name("FOO_BAR_BAZ"));
        assert!(is_valid_raw_name("A2B"));
    }

    #[test]
    fn invalid_raw_names() {
        assert!(!is_valid_raw_name("A"));
        assert!(!is_valid_raw_name("findings"));
        assert!(!is_valid_raw_name("Findings"));
        assert!(!is_valid_raw_name("1FOO"));
        assert!(!is_valid_raw_name("FOO_"));
        assert!(!is_valid_raw_name("_FOO"));
        assert!(!is_valid_raw_name(""));
        assert!(!is_valid_raw_name("FOO BAR"));
    }

    #[test]
    fn version_suffix_detection() {
        assert!(has_version_suffix("FINDINGS_2"));
        assert!(has_version_suffix("FINDINGS_123"));
        assert!(has_version_suffix("A_0"));
        assert!(!has_version_suffix("FINDINGS"));
        assert!(!has_version_suffix("API_DESIGN"));
        assert!(!has_version_suffix("FOO_BAR"));
    }

    // -- changelog serialization --

    #[test]
    fn changelog_bootstrap_roundtrip() {
        let entry = ChangelogEntry::Bootstrap {
            ts: "2026-03-25T14:00:00Z".to_owned(),
            raw: "REQUIREMENTS_1.md".to_owned(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""op":"bootstrap""#));
        assert!(json.contains(r#""raw":"REQUIREMENTS_1.md""#));
        let parsed: ChangelogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn changelog_record_roundtrip() {
        let entry = ChangelogEntry::Record {
            ts: "2026-03-25T15:30:00Z".to_owned(),
            raw: "FINDINGS_1.md".to_owned(),
            derived_modified: vec!["DESIGN.md".to_owned(), "FINDINGS.md".to_owned()],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ChangelogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn changelog_reorganize_roundtrip() {
        let entry = ChangelogEntry::Reorganize {
            ts: "2026-03-26T09:00:00Z".to_owned(),
            merged: vec!["FINDINGS.md".to_owned()],
            restructured: vec!["PROJECT.md".to_owned()],
            deleted: vec![],
        };
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ChangelogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, parsed);
    }

    #[test]
    fn changelog_deserialize_from_spec_examples() {
        let lines = [
            r#"{"ts":"2026-03-25T14:00:00Z","op":"bootstrap","raw":"REQUIREMENTS_1.md"}"#,
            r#"{"ts":"2026-03-25T15:30:00Z","op":"record","raw":"FINDINGS_1.md","derived_modified":["DESIGN.md","FINDINGS.md"]}"#,
            r#"{"ts":"2026-03-26T09:00:00Z","op":"reorganize","merged":["FINDINGS.md"],"restructured":["PROJECT.md"],"deleted":[]}"#,
        ];
        for line in &lines {
            let _entry: ChangelogEntry = serde_json::from_str(line).unwrap();
        }
    }

    // -- storage integration tests --

    #[test]
    fn not_initialized_initially() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        assert!(!storage.is_initialized());
    }

    #[test]
    fn create_directories_marks_initialized() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        assert!(storage.is_initialized());
        assert!(storage.raw_dir().is_dir());
        assert!(storage.derived_dir().is_dir());
    }

    #[test]
    fn create_directories_under_file_fails() {
        // Place a regular file where create_directories expects a directory
        // ancestor. create_dir_all cannot create subdirectories under a file.
        let tmp = TempDir::new().unwrap();
        let blocker = tmp.path().join("blocker");
        fs::write(&blocker, "I am a file").unwrap();

        let storage = Storage::new(blocker);
        let result = storage.create_directories();
        assert!(matches!(result, Err(StorageError::Io(_))));
    }

    #[test]
    fn changelog_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());

        let e1 = ChangelogEntry::Bootstrap {
            ts: "2026-03-25T14:00:00Z".to_owned(),
            raw: "REQUIREMENTS_1.md".to_owned(),
        };
        let e2 = ChangelogEntry::Record {
            ts: "2026-03-25T15:00:00Z".to_owned(),
            raw: "FINDINGS_1.md".to_owned(),
            derived_modified: vec!["DESIGN.md".to_owned()],
        };

        storage.append_changelog(&e1).unwrap();
        storage.append_changelog(&e2).unwrap();

        let entries = storage.read_changelog().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], e1);
        assert_eq!(entries[1], e2);
    }

    #[test]
    fn read_changelog_corrupt_data() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        fs::write(storage.changelog_path(), "this is not json at all\n").unwrap();

        let result = storage.read_changelog();
        assert!(
            matches!(result, Err(StorageError::Io(ref e)) if e.kind() == io::ErrorKind::InvalidData)
        );
    }

    // -- version scanning --

    #[test]
    fn version_scanning_empty() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let versions = storage.scan_versions("FINDINGS").unwrap();
        assert!(versions.is_empty());
        assert_eq!(versions.last().map_or(1, |v| v.version + 1), 1);
    }

    #[test]
    fn version_scanning_with_files() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        storage.write_raw("FINDINGS", 1, "v1 content").unwrap();
        storage.write_raw("FINDINGS", 2, "v2 content").unwrap();
        storage.write_raw("OTHER", 1, "other v1").unwrap();

        let versions = storage.scan_versions("FINDINGS").unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version, 1);
        assert_eq!(versions[1].version, 2);

        let findings_versions = storage.scan_versions("FINDINGS").unwrap();
        assert_eq!(findings_versions.last().map_or(1, |v| v.version + 1), 3);
        let other_versions = storage.scan_versions("OTHER").unwrap();
        assert_eq!(other_versions.last().map_or(1, |v| v.version + 1), 2);
        let absent_versions = storage.scan_versions("ABSENT").unwrap();
        assert_eq!(absent_versions.last().map_or(1, |v| v.version + 1), 1);
    }

    #[test]
    fn write_raw_versioned_new_success() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let fname = storage
            .write_raw_versioned("REQUIREMENTS", "hello", true)
            .unwrap();
        assert_eq!(fname, "REQUIREMENTS_1.md");

        let content = storage.read_raw(&fname).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn write_raw_versioned_new_conflict() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        storage.write_raw("FINDINGS", 1, "existing").unwrap();

        let result = storage.write_raw_versioned("FINDINGS", "new", true);
        assert!(matches!(result, Err(StorageError::VersionConflict(_))));
    }

    #[test]
    fn write_raw_versioned_append_success() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();
        storage.write_raw("FINDINGS", 1, "v1").unwrap();

        let fname = storage
            .write_raw_versioned("FINDINGS", "v2", false)
            .unwrap();
        assert_eq!(fname, "FINDINGS_2.md");
    }

    #[test]
    fn write_raw_versioned_append_not_found() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let result = storage.write_raw_versioned("FINDINGS", "v1", false);
        assert!(matches!(result, Err(StorageError::DocumentNotFound(_))));
    }

    #[test]
    fn write_raw_versioned_invalid_name() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        assert!(matches!(
            storage.write_raw_versioned("findings", "x", true),
            Err(StorageError::InvalidName(_))
        ));
        assert!(matches!(
            storage.write_raw_versioned("FINDINGS_2", "x", true),
            Err(StorageError::InvalidName(_))
        ));
        assert!(matches!(
            storage.write_raw_versioned("A", "x", true),
            Err(StorageError::InvalidName(_))
        ));
    }

    #[test]
    fn read_raw_not_found() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let result = storage.read_raw("NONEXISTENT_1.md");
        assert!(
            matches!(result, Err(StorageError::DocumentNotFound(ref f)) if f == "NONEXISTENT_1.md")
        );
    }

    // -- derived listing --

    #[test]
    fn list_derived_with_files() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("ZEBRA.md"), "z").unwrap();
        fs::write(derived.join("ALPHA.md"), "a").unwrap();
        fs::write(derived.join("MIDDLE.md"), "m").unwrap();

        let docs = storage.list_derived().unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0].filename, "ALPHA.md");
        assert_eq!(docs[1].filename, "MIDDLE.md");
        assert_eq!(docs[2].filename, "ZEBRA.md");
    }

    #[test]
    fn list_derived_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let docs = storage.list_derived().unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn list_derived_no_dir() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        // Do NOT call create_directories — derived/ does not exist.

        let docs = storage.list_derived().unwrap();
        assert!(docs.is_empty());
    }

    #[test]
    fn list_derived_skips_directories() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("REAL.md"), "content").unwrap();
        fs::create_dir(derived.join("SUBDIR")).unwrap();

        let docs = storage.list_derived().unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].filename, "REAL.md");
    }

    // -- derived validation --

    #[test]
    fn validate_derived_good_files() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(
            derived.join("PROJECT.md"),
            "# Project Overview\n<!-- scope: top-level project index -->\n\nContent here.\n",
        )
        .unwrap();
        fs::write(
            derived.join("DESIGN.md"),
            "# Design Decisions\n<!-- scope: architecture and design -->\n\nContent.\n",
        )
        .unwrap();

        let warnings = storage.validate_derived().unwrap();
        assert!(warnings.is_empty());
    }

    #[test]
    fn validate_derived_bad_filename() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("lowercase.md"), "# Title\n<!-- scope: x -->\n").unwrap();

        let warnings = storage.validate_derived().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("filename"));
    }

    #[test]
    fn validate_derived_missing_header() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("PROJECT.md"), "No title here\n").unwrap();

        let warnings = storage.validate_derived().unwrap();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].reason.contains("title"));
        assert!(warnings[1].reason.contains("scope"));
    }

    #[test]
    fn validate_derived_missing_scope() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("PROJECT.md"), "# Title\nNo scope comment.\n").unwrap();

        let warnings = storage.validate_derived().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("scope"));
    }

    #[test]
    fn validate_derived_scope_must_be_on_second_line() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();
        // Valid title on line 1, blank line on line 2, valid scope comment on line 3.
        fs::write(derived.join("PROJECT.md"), "# Title\n\n<!-- scope: x -->\n").unwrap();

        let warnings = storage.validate_derived().unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].reason.contains("scope"));
    }

    #[test]
    #[cfg(unix)]
    fn validate_derived_unreadable_file_warns_and_continues() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let derived = storage.derived_dir();

        // A good file that should still be validated
        fs::write(
            derived.join("GOOD.md"),
            "# Good File\n<!-- scope: test -->\n",
        )
        .unwrap();

        // An unreadable file
        let unreadable = derived.join("UNREADABLE.md");
        fs::write(&unreadable, "# Title\n<!-- scope: x -->\n").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        // Skip test if running as root (permissions don't block root reads)
        if fs::read_to_string(&unreadable).is_ok() {
            fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
            return;
        }

        let warnings = storage.validate_derived().unwrap();

        // Restore permissions so temp dir cleanup succeeds
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();

        // The unreadable file should produce a warning, not abort validation
        assert!(
            warnings
                .iter()
                .any(|w| w.filename == "UNREADABLE.md" && w.reason.contains("unable to read file"))
        );
        // The good file should still have been validated (no warnings for it)
        assert!(!warnings.iter().any(|w| w.filename == "GOOD.md"));
    }

    // -- inventory --

    #[test]
    fn inventory_lists_all() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        storage.write_raw("FINDINGS", 1, "v1").unwrap();
        storage.write_raw("FINDINGS", 2, "v2").unwrap();
        storage.write_raw("REQUIREMENTS", 1, "req").unwrap();

        let derived = storage.derived_dir();
        fs::write(derived.join("PROJECT.md"), "# P\n<!-- scope: x -->\n").unwrap();
        fs::write(derived.join("DESIGN.md"), "# D\n<!-- scope: y -->\n").unwrap();

        let inv = storage.inventory().unwrap();
        assert_eq!(inv.raw.len(), 3);
        assert_eq!(inv.derived.len(), 2);
    }

    #[test]
    fn inventory_ignores_non_matching_raw_files() {
        let tmp = TempDir::new().unwrap();
        let storage = Storage::new(tmp.path().to_path_buf());
        storage.create_directories().unwrap();

        let raw = storage.raw_dir();
        // Valid versioned files
        storage.write_raw("FINDINGS", 1, "v1").unwrap();
        storage.write_raw("REQUIREMENTS", 1, "req").unwrap();
        // Non-matching files that should be ignored
        fs::write(raw.join("readme.txt"), "ignored").unwrap();
        fs::write(raw.join("notes.md"), "ignored").unwrap();
        fs::write(raw.join("lowercase_1.md"), "ignored").unwrap();
        fs::create_dir(raw.join("SUBDIR")).unwrap();

        let inv = storage.inventory().unwrap();
        assert_eq!(inv.raw.len(), 2);
        let names: Vec<&str> = inv.raw.iter().map(|r| r.filename.as_str()).collect();
        assert!(names.contains(&"FINDINGS_1.md"));
        assert!(names.contains(&"REQUIREMENTS_1.md"));
    }

    // -- timestamp --

    #[test]
    fn utc_timestamp_format() {
        let ts = utc_now_iso8601();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
    }

    // -- days_to_civil --

    #[test]
    fn test_days_to_civil() {
        // Day 0: Unix epoch
        assert_eq!(days_to_civil(0), (1970, 1, 1));
        // Day 1
        assert_eq!(days_to_civil(1), (1970, 1, 2));
        // Day 365: first day of 1971
        assert_eq!(days_to_civil(365), (1971, 1, 1));
        // Day 10957: Y2K
        assert_eq!(days_to_civil(10957), (2000, 1, 1));
        // Day 11016: leap day 2000
        assert_eq!(days_to_civil(11016), (2000, 2, 29));
        // Day 11017: after leap day in 2000
        assert_eq!(days_to_civil(11017), (2000, 3, 1));
        // Day 19448: 2023-04-01
        assert_eq!(days_to_civil(19448), (2023, 4, 1));
        // Day 20537: 2026-03-25
        assert_eq!(days_to_civil(20537), (2026, 3, 25));
    }
}
