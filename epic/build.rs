// build.rs — Download prebuilt NuShell and ripgrep binaries for the target platform.
//
// On `cargo build`, this script downloads release binaries matching the target
// platform, verifies SHA-256 checksums, extracts them from archives, and caches
// them under `target/nu-cache/`. The runtime uses `NU_CACHE_DIR` (emitted as a
// compile-time env var) to locate both binaries.
//
// Set `NU_SKIP_DOWNLOAD=1` or `RG_SKIP_DOWNLOAD=1` to skip individual
// downloads (offline builds, CI with pre-populated cache). The runtime falls
// back to PATH lookup.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const NU_VERSION: &str = "0.111.0";
const RG_VERSION: &str = "14.1.1";

struct PlatformAsset {
    asset_name: &'static str,
    sha256: &'static str,
    binary_name: &'static str,
}

fn nu_platform_asset(os: &str, arch: &str) -> PlatformAsset {
    match (os, arch) {
        ("windows", "x86_64") => PlatformAsset {
            asset_name: "nu-0.111.0-x86_64-pc-windows-msvc.zip",
            sha256: "4efd0a72ce26052961183aa3ecb8dce17bb6c43903392bc3521a9fda4e6127b2",
            binary_name: "nu.exe",
        },
        ("windows", "aarch64") => PlatformAsset {
            asset_name: "nu-0.111.0-aarch64-pc-windows-msvc.zip",
            sha256: "e4fe1309d3f001d6d05f6ee2a8e25bee25d2dd03ba33db1bca4367a69d7891b8",
            binary_name: "nu.exe",
        },
        ("linux", "x86_64") => PlatformAsset {
            asset_name: "nu-0.111.0-x86_64-unknown-linux-gnu.tar.gz",
            sha256: "aa5376efaa5f2da98ebae884b901af6504dc8291acf5f4147ac994e9d03cd1ba",
            binary_name: "nu",
        },
        ("linux", "aarch64") => PlatformAsset {
            asset_name: "nu-0.111.0-aarch64-unknown-linux-gnu.tar.gz",
            sha256: "ff72150fefcac7c990fa0f2e04550d51b609274cbd0a2831335e6975bd2079c8",
            binary_name: "nu",
        },
        ("macos", "x86_64") => PlatformAsset {
            asset_name: "nu-0.111.0-x86_64-apple-darwin.tar.gz",
            sha256: "20dae71461c4d432531f78e5dfcd1f3cf5919ebbbafd10a95e8a2925532b721a",
            binary_name: "nu",
        },
        ("macos", "aarch64") => PlatformAsset {
            asset_name: "nu-0.111.0-aarch64-apple-darwin.tar.gz",
            sha256: "260e59f7f9ac65cad4624cd45c11e38ac8aed7d0d7d027ad2d39f50d2373b274",
            binary_name: "nu",
        },
        _ => {
            panic!("unsupported target for nu: os={os} arch={arch}");
        }
    }
}

fn rg_platform_asset(os: &str, arch: &str) -> PlatformAsset {
    match (os, arch) {
        ("windows", "x86_64" | "aarch64") => PlatformAsset {
            // No aarch64-pc-windows-msvc build exists for rg. Windows ARM64
            // runs x86_64 binaries via built-in emulation.
            asset_name: "ripgrep-14.1.1-x86_64-pc-windows-msvc.zip",
            sha256: "d0f534024c42afd6cb4d38907c25cd2b249b79bbe6cc1dbee8e3e37c2b6e25a1",
            binary_name: "rg.exe",
        },
        ("linux", "x86_64") => PlatformAsset {
            asset_name: "ripgrep-14.1.1-x86_64-unknown-linux-musl.tar.gz",
            sha256: "4cf9f2741e6c465ffdb7c26f38056a59e2a2544b51f7cc128ef28337eeae4d8e",
            binary_name: "rg",
        },
        ("linux", "aarch64") => PlatformAsset {
            asset_name: "ripgrep-14.1.1-aarch64-unknown-linux-gnu.tar.gz",
            sha256: "c827481c4ff4ea10c9dc7a4022c8de5db34a5737cb74484d62eb94a95841ab2f",
            binary_name: "rg",
        },
        ("macos", "x86_64") => PlatformAsset {
            asset_name: "ripgrep-14.1.1-x86_64-apple-darwin.tar.gz",
            sha256: "fc87e78f7cb3fea12d69072e7ef3b21509754717b746368fd40d88963630e2b3",
            binary_name: "rg",
        },
        ("macos", "aarch64") => PlatformAsset {
            asset_name: "ripgrep-14.1.1-aarch64-apple-darwin.tar.gz",
            sha256: "24ad76777745fbff131c8fbc466742b011f925bfa4fffa2ded6def23b5b937be",
            binary_name: "rg",
        },
        _ => {
            panic!("unsupported target for rg: os={os} arch={arch}");
        }
    }
}

/// Walk up from `OUT_DIR` to find the `target/` directory.
fn find_target_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    // OUT_DIR is typically target/<profile>/build/<crate>-<hash>/out
    // Walk up looking for a directory that contains a `.cargo-lock` or is named `target`.
    let mut dir = out_dir.as_path();
    while let Some(parent) = dir.parent() {
        if dir.file_name().is_some_and(|n| n == "target") {
            return dir.to_path_buf();
        }
        dir = parent;
    }
    panic!(
        "cannot find `target/` directory walking up from OUT_DIR={}",
        out_dir.display()
    )
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn download(url: &str, dest: &Path) -> Result<(), String> {
    eprintln!("Downloading {url}");
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {e}"))?;

    let mut body = response.into_body().into_reader();
    let mut file =
        fs::File::create(dest).map_err(|e| format!("failed to create {}: {e}", dest.display()))?;

    io::copy(&mut body, &mut file)
        .map_err(|e| format!("failed to write {}: {e}", dest.display()))?;
    file.flush()
        .map_err(|e| format!("failed to flush {}: {e}", dest.display()))?;

    Ok(())
}

fn extract_tar_gz(archive_path: &Path, binary_name: &str, dest: &Path) -> Result<(), String> {
    let file =
        fs::File::open(archive_path).map_err(|e| format!("failed to open archive: {e}"))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    for entry in archive
        .entries()
        .map_err(|e| format!("failed to read tar entries: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("bad tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("bad tar entry path: {e}"))?;

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == binary_name {
            let mut out = fs::File::create(dest)
                .map_err(|e| format!("failed to create {}: {e}", dest.display()))?;
            io::copy(&mut entry, &mut out)
                .map_err(|e| format!("failed to extract {binary_name}: {e}"))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(dest, fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("failed to set permissions: {e}"))?;
            }

            return Ok(());
        }
    }

    Err(format!("{binary_name} not found in archive"))
}

fn extract_zip(archive_path: &Path, binary_name: &str, dest: &Path) -> Result<(), String> {
    let file =
        fs::File::open(archive_path).map_err(|e| format!("failed to open archive: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("failed to read zip: {e}"))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("bad zip entry: {e}"))?;

        let file_name = Path::new(entry.name())
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == binary_name {
            let mut out = fs::File::create(dest)
                .map_err(|e| format!("failed to create {}: {e}", dest.display()))?;
            io::copy(&mut entry, &mut out)
                .map_err(|e| format!("failed to extract {binary_name}: {e}"))?;
            return Ok(());
        }
    }

    Err(format!("{binary_name} not found in archive"))
}

/// Download, verify, extract, and cache a single binary.
fn download_and_cache(
    label: &str,
    version: &str,
    download_base_url: &str,
    asset: &PlatformAsset,
    cache_dir: &Path,
    skip_env_var: &str,
) {
    let binary_path = cache_dir.join(asset.binary_name);
    let sentinel = cache_dir.join(format!(".verified-{}", asset.asset_name));

    // Already downloaded and verified.
    if sentinel.exists() && binary_path.exists() {
        return;
    }

    // Offline escape hatch.
    if std::env::var(skip_env_var).is_ok_and(|v| v == "1") {
        eprintln!("{skip_env_var}=1: skipping {label} binary download");
        return;
    }

    let url = format!("{download_base_url}/{}", asset.asset_name);
    let archive_path = cache_dir.join(asset.asset_name);
    download(&url, &archive_path)
        .unwrap_or_else(|e| panic!("failed to download {label} binary: {e}"));

    // Verify archive checksum.
    let actual_hash =
        sha256_file(&archive_path).expect("failed to compute SHA-256 of downloaded archive");
    assert_eq!(
        actual_hash, asset.sha256,
        "SHA-256 mismatch for {}: expected {}, got {actual_hash}",
        asset.asset_name, asset.sha256
    );

    // Extract the binary from the archive.
    if asset.asset_name.ends_with(".tar.gz") {
        extract_tar_gz(&archive_path, asset.binary_name, &binary_path)
            .unwrap_or_else(|e| panic!("failed to extract {label} binary from tar.gz: {e}"));
    } else {
        extract_zip(&archive_path, asset.binary_name, &binary_path)
            .unwrap_or_else(|e| panic!("failed to extract {label} binary from zip: {e}"));
    }

    // Clean up the archive.
    let _ = fs::remove_file(&archive_path);

    // Write sentinel so we don't re-download.
    fs::write(&sentinel, asset.sha256).expect("failed to write sentinel file");

    eprintln!(
        "{label} {version} binary cached at {}",
        binary_path.display()
    );
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NU_SKIP_DOWNLOAD");
    println!("cargo:rerun-if-env-changed=RG_SKIP_DOWNLOAD");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");
    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH not set");

    let cache_dir = find_target_dir().join("nu-cache");
    fs::create_dir_all(&cache_dir).expect("failed to create nu-cache directory");

    // Emit cache dir for runtime lookup (both binaries share the same directory).
    println!(
        "cargo:rustc-env=NU_CACHE_DIR={}",
        cache_dir.to_str().expect("cache dir not valid UTF-8")
    );

    // NuShell
    let nu_asset = nu_platform_asset(&target_os, &target_arch);
    download_and_cache(
        "NuShell",
        NU_VERSION,
        &format!("https://github.com/nushell/nushell/releases/download/{NU_VERSION}"),
        &nu_asset,
        &cache_dir,
        "NU_SKIP_DOWNLOAD",
    );

    // ripgrep
    let rg_asset = rg_platform_asset(&target_os, &target_arch);
    download_and_cache(
        "ripgrep",
        RG_VERSION,
        &format!("https://github.com/BurntSushi/ripgrep/releases/download/{RG_VERSION}"),
        &rg_asset,
        &cache_dir,
        "RG_SKIP_DOWNLOAD",
    );

    // Write epic nu config files to the cache directory.
    write_nu_config_files(&cache_dir);
}

/// Write `epic_config.nu` and `epic_env.nu` to the cache directory.
///
/// These are passed to `nu --mcp --config <path> --env-config <path>` at
/// runtime so that `epic read`, `epic write`, etc. are available immediately
/// in the MCP session without an evaluate preamble.
fn write_nu_config_files(cache_dir: &Path) {
    for (name, content) in [("epic_config.nu", EPIC_CONFIG_NU), ("epic_env.nu", EPIC_ENV_NU)] {
        let path = cache_dir.join(name);
        fs::write(&path, content)
            .unwrap_or_else(|e| panic!("failed to write {name} to {}: {e}", path.display()));
    }
}

/// `NuShell` config file containing epic custom command definitions.
///
/// Loaded via `nu --config <path>`. Defines `epic read`, `epic write`,
/// `epic edit`, `epic glob`, `epic grep` as subcommands.
const EPIC_CONFIG_NU: &str = r#"
# Epic custom commands — loaded via --config flag.
# Do not edit; regenerated by build.rs on each build.

# Parent command — lists all epic subcommands via help
def epic [] { help epic }

# Read file contents, return structured record
def "epic read" [
    path: string
    --offset: int    # 1-based line number to start from
    --limit: int     # max lines to return
] {
    let full = ($path | path expand)
    let meta = (ls $full | first)
    if $meta.size > 256KiB {
        error make { msg: $"File too large: ($meta.size), max 256 KiB" }
    }
    let all_lines = (open $full --raw | lines)
    let total = ($all_lines | length)
    let start = if ($offset | is-empty) { 0 } else { [($offset - 1) 0] | math max }
    let selected = ($all_lines | skip $start)
    let selected = if ($limit | is-empty) { $selected } else { $selected | take $limit }
    let numbered = ($selected | enumerate | each { |row|
        {line: ($row.index + $start + 1), text: $row.item}
    })
    {
        path: $full,
        size: $meta.size,
        total_lines: $total,
        offset: ($start + 1),
        lines_returned: ($numbered | length),
        lines: $numbered,
    }
}

# Write content to file, return structured record
def "epic write" [path: string, content: string] {
    let byte_size = ($content | encode utf-8 | bytes length | into filesize)
    if $byte_size > 1MiB {
        error make { msg: $"Content too large: ($byte_size), max 1 MiB" }
    }
    let full = ($path | path expand)
    let parent = ($full | path dirname)
    mkdir $parent
    $content | save --force $full
    {
        path: $full,
        bytes_written: $byte_size,
    }
}

# Exact substring replacement, return structured record
def "epic edit" [
    path: string
    old_string: string
    new_string: string
    --replace-all    # replace all occurrences instead of requiring uniqueness
] {
    let full = ($path | path expand)
    let content = (open $full --raw)
    let parts = ($content | split row $old_string)
    let count = (($parts | length) - 1)
    if $count == 0 { error make { msg: "old_string not found in file" } }
    if not $replace_all {
        if $count > 1 {
            error make { msg: $"old_string found ($count) times, must be unique" }
        }
        ($content | str replace $old_string $new_string) | save --force $full
        { path: $full, replacements: 1 }
    } else {
        ($content | str replace --all $old_string $new_string) | save --force $full
        { path: $full, replacements: $count }
    }
}

# Find files by pattern, return list<string>, 1000 result cap
def "epic glob" [
    pattern: string
    --path: string   # directory to search in
] {
    let dir = if ($path | is-empty) { "." } else { $path }
    do { cd $dir; glob $pattern } | take 1000
}

# Search file contents via rg, return structured record.
# Requires rg (ripgrep) binary in the sandbox.
def "epic grep" [
    pattern: string
    --path: string
    --output-mode: string          # content, files_with_matches (default), count
    --glob: string                 # file name filter
    --type: string                 # file type filter (js, py, rust...)
    --case-insensitive             # case insensitive search
    --no-line-numbers              # disable line numbers
    --context-after: int           # lines after match
    --context-before: int          # lines before match
    --context: int                 # lines before and after match
    --multiline                    # match across lines
    --head-limit: int              # limit result count
] {
    let search_path = if ($path | is-empty) { "." } else { $path }
    let mode = if ($output_mode | is-empty) { "files_with_matches" } else { $output_mode }

    mut args = ["--color=never"]

    # Output mode
    if $mode == "files_with_matches" { $args = ($args | append "-l") }
    if $mode == "count" { $args = ($args | append "-c") }

    # Filters
    if not ($glob | is-empty) { $args = ($args | append ["--glob" $glob]) }
    if not ($type | is-empty) { $args = ($args | append ["--type" $type]) }
    if $case_insensitive { $args = ($args | append "-i") }
    if $multiline { $args = ($args | append "--multiline") }

    # Line numbers (default on for content mode)
    if $mode == "content" {
        if $no_line_numbers {
            $args = ($args | append "--no-line-number")
        } else {
            $args = ($args | append "-n")
        }
    }

    # Context lines
    if not ($context_after | is-empty) { $args = ($args | append ["-A" ($context_after | into string)]) }
    if not ($context_before | is-empty) { $args = ($args | append ["-B" ($context_before | into string)]) }
    if not ($context | is-empty) { $args = ($args | append ["-C" ($context | into string)]) }

    # -- separates flags from pattern; search_path is positional after pattern
    $args = ($args | append ["--" $pattern $search_path])

    let result = (^rg ...$args | complete)

    let output_lines = ($result.stdout | lines)
    let output_lines = if ($head_limit | is-empty) { $output_lines } else { $output_lines | take $head_limit }

    if $result.exit_code >= 2 {
        error make { msg: $"rg failed \(exit code ($result.exit_code)\): ($result.stderr)" }
    }

    {
        exit_code: $result.exit_code,
        output: ($output_lines | str join "\n"),
    }
}
"#;

/// `NuShell` env config file. Sets up PATH for bundled rg binary.
///
/// Loaded via `nu --env-config <path>`. The rg binary directory is injected
/// at runtime via the `EPIC_RG_DIR` environment variable set by `NuSession`.
const EPIC_ENV_NU: &str = r#"
# Epic environment config — loaded via --env-config flag.
# Do not edit; regenerated by build.rs on each build.

# Add bundled rg binary directory to PATH if EPIC_RG_DIR is set.
if "EPIC_RG_DIR" in $env {
    if $nu.os-info.name == "windows" {
        $env.Path = ($env.Path | prepend $env.EPIC_RG_DIR)
    } else {
        $env.PATH = ($env.PATH | prepend $env.EPIC_RG_DIR)
    }
}
"#;
