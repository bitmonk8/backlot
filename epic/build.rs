// build.rs — Download a prebuilt NuShell binary for the target platform.
//
// On `cargo build`, this script downloads the NuShell 0.111.0 release binary
// matching the target platform, verifies its SHA-256 checksum, extracts it
// from the archive, and caches it under `target/nu-cache/`. The runtime uses
// `NU_CACHE_DIR` (emitted as a compile-time env var) to locate the binary.
//
// Set `NU_SKIP_DOWNLOAD=1` to skip the download (offline builds, CI with
// pre-populated cache). The runtime falls back to PATH lookup.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const NU_VERSION: &str = "0.111.0";

struct PlatformAsset {
    asset_name: &'static str,
    sha256: &'static str,
    binary_name: &'static str,
}

fn platform_asset(os: &str, arch: &str) -> PlatformAsset {
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
            panic!("unsupported target: os={os} arch={arch}");
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
    panic!("cannot find `target/` directory walking up from OUT_DIR={}", out_dir.display())
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
    let file = fs::File::open(archive_path)
        .map_err(|e| format!("failed to open archive: {e}"))?;
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
    let file = fs::File::open(archive_path)
        .map_err(|e| format!("failed to open archive: {e}"))?;
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

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=NU_SKIP_DOWNLOAD");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");
    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH not set");

    let cache_dir = find_target_dir().join("nu-cache");
    fs::create_dir_all(&cache_dir).expect("failed to create nu-cache directory");

    let asset = platform_asset(&target_os, &target_arch);
    let binary_path = cache_dir.join(asset.binary_name);
    let sentinel = cache_dir.join(format!(".verified-{}", asset.asset_name));

    // Emit cache dir for runtime lookup.
    println!(
        "cargo:rustc-env=NU_CACHE_DIR={}",
        cache_dir.to_str().expect("cache dir not valid UTF-8")
    );

    // If sentinel exists, the binary was already downloaded and verified.
    if sentinel.exists() && binary_path.exists() {
        return;
    }

    // Offline escape hatch.
    if std::env::var("NU_SKIP_DOWNLOAD").is_ok_and(|v| v == "1") {
        eprintln!("NU_SKIP_DOWNLOAD=1: skipping NuShell binary download");
        return;
    }

    let url = format!(
        "https://github.com/nushell/nushell/releases/download/{NU_VERSION}/{}",
        asset.asset_name
    );

    let archive_path = cache_dir.join(asset.asset_name);
    download(&url, &archive_path).expect("failed to download NuShell binary");

    // Verify archive checksum.
    let actual_hash =
        sha256_file(&archive_path).expect("failed to compute SHA-256 of downloaded archive");
    assert_eq!(
        actual_hash, asset.sha256,
        "SHA-256 mismatch for {}: expected {}, got {actual_hash}",
        asset.asset_name, asset.sha256
    );

    // Extract the nu binary from the archive.
    if asset.asset_name.ends_with(".tar.gz") {
        extract_tar_gz(&archive_path, asset.binary_name, &binary_path)
            .expect("failed to extract NuShell binary from tar.gz");
    } else {
        extract_zip(&archive_path, asset.binary_name, &binary_path)
            .expect("failed to extract NuShell binary from zip");
    }

    // Clean up the archive.
    let _ = fs::remove_file(&archive_path);

    // Write sentinel so we don't re-download.
    fs::write(&sentinel, asset.sha256).expect("failed to write sentinel file");

    eprintln!(
        "NuShell {NU_VERSION} binary cached at {}",
        binary_path.display()
    );
}
