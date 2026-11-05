// build.rs — embeds the platform pdfium library into the binary for the
// `bundled` feature (a no-op when the feature is off).
//
// Library resolution (first match wins):
//   1. `PDFIUM_BUNDLE_LIB` — explicit path you supply (CI / air-gapped).
//   2. Auto-download from bblanchon/pdfium-binaries via `curl`, cached under
//      $CARGO_HOME/pdfium-bundled/{VERSION}/{os}-{arch}/ (override the root
//      with `PDFIUM_BUILD_CACHE_DIR`).
//
// Whatever the source, the bytes that reach `include_bytes!` must hash to the
// `lib_sha256` pinned for the target in src/platform.rs, and a downloaded
// archive must hash to its `archive_sha256` before it is unpacked. Only an
// explicit `PDFIUM_BUNDLE_LIB` can opt out, via `PDFIUM_ALLOW_UNVERIFIED_LIB`.

use std::path::{Path, PathBuf};

// PDFIUM_VERSION, PDFIUM_API_FLOOR, BASE_URL, PlatformInfo, and platform_for()
// are shared verbatim with the library via include!, so the build-time download
// and the runtime bind can never target different pdfium builds. See
// src/platform.rs. The digest helpers come in the same way, so the embed and
// the bind enforce the same pins.
include!("src/platform.rs");
include!("src/integrity.rs");

// ── Cache directory ──────────────────────────────────────────────────────────

fn build_cache_dir(target_os: &str, target_arch: &str) -> PathBuf {
    if let Ok(v) = std::env::var("PDFIUM_BUILD_CACHE_DIR") {
        return PathBuf::from(v)
            .join(PDFIUM_VERSION)
            .join(format!("{target_os}-{target_arch}"));
    }

    let cargo_home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        home.join(".cargo")
    });

    cargo_home
        .join("pdfium-bundled")
        .join(PDFIUM_VERSION)
        .join(format!("{target_os}-{target_arch}"))
}

// ── Download helper ──────────────────────────────────────────────────────────

fn download_file(url: &str, dest: &Path) {
    println!(
        "cargo:warning=pdfium-bundled[bundled]: downloading {} (chromium/{PDFIUM_VERSION})…",
        url.rsplit('/').next().unwrap_or(url)
    );

    let result = std::process::Command::new("curl")
        .args(["-L", "-f", "-s", "--retry", "3", "-o", &dest.to_string_lossy(), url])
        .status();

    match result {
        Ok(s) if s.success() => return,
        Ok(s) => {
            println!("cargo:warning=pdfium-bundled[bundled]: curl exited {s}, trying PowerShell…")
        }
        Err(e) => println!("cargo:warning=pdfium-bundled[bundled]: curl unavailable ({e}), trying PowerShell…"),
    }

    // PowerShell fallback (Windows without curl in PATH)
    let ps = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!(
                "Invoke-WebRequest -Uri '{url}' -OutFile '{}' -UseBasicParsing",
                dest.display()
            ),
        ])
        .status();

    if matches!(ps, Ok(s) if s.success()) {
        return;
    }

    panic!(
        "\n\
         pdfium-bundled[bundled]: failed to auto-download pdfium.\n\
         Both curl and PowerShell failed.\n\n\
         Quick fix — download manually and set:\n\
           export PDFIUM_BUNDLE_LIB=/path/to/libpdfium\n\n\
         Pre-built libraries (chromium/{PDFIUM_VERSION}):\n\
           https://github.com/bblanchon/pdfium-binaries/releases"
    );
}

// ── Extraction helper ────────────────────────────────────────────────────────

fn extract_lib(tgz_path: &Path, lib_path_in_archive: &str, dest: &Path) {
    let file = std::fs::File::open(tgz_path)
        .unwrap_or_else(|e| panic!("pdfium-bundled: cannot open {}: {e}", tgz_path.display()));
    let gz = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(gz);

    for entry_result in archive
        .entries()
        .expect("pdfium-bundled: failed to iterate tar archive")
    {
        let mut entry = entry_result.expect("pdfium-bundled: failed to read tar entry");
        let path = entry
            .path()
            .expect("pdfium-bundled: invalid tar entry path")
            .to_path_buf();

        if path.to_str() == Some(lib_path_in_archive) {
            entry
                .unpack(dest)
                .unwrap_or_else(|e| panic!("pdfium-bundled: failed to extract '{lib_path_in_archive}': {e}"));
            return;
        }
    }

    panic!(
        "pdfium-bundled: '{lib_path_in_archive}' not found in '{}'.\n\
         The upstream archive layout may have changed.\n\
         Set PDFIUM_BUNDLE_LIB to provide the library manually.",
        tgz_path.display()
    );
}

// ── Verification helpers ─────────────────────────────────────────────────────

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("pdfium-bundled: cannot read {}: {e}", path.display()))
}

/// Checks a file against a pinned digest, returning the mismatch instead of
/// panicking so each call site can decide between refusing and refetching.
fn check_file(what: &'static str, expected: &str, path: &Path) -> Result<(), DigestMismatch> {
    verify_digest(what, expected, &read_bytes(path))
}

/// The one place an unverified library is accepted: the caller supplied it
/// explicitly and set the opt-out. Loud on purpose.
fn accept_unverified(path: &Path) -> PathBuf {
    println!(
        "cargo:warning=pdfium-bundled[bundled]: {ALLOW_UNVERIFIED_ENV} is set — embedding {} WITHOUT verifying \
         its digest. The resulting binary loads native code this crate has not vouched for.",
        path.display()
    );
    path.to_path_buf()
}

// ── Path resolution ──────────────────────────────────────────────────────────

fn resolve_lib(target_os: &str, target_arch: &str) -> PathBuf {
    // The platform table is needed even for an explicit path: it holds the
    // digest the path must match.
    let bundle = platform_for(target_os, target_arch);

    if let Ok(p) = std::env::var("PDFIUM_BUNDLE_LIB")
        && !p.is_empty()
    {
        let path = PathBuf::from(&p);
        if !path.exists() {
            panic!(
                "pdfium-bundled: PDFIUM_BUNDLE_LIB={p} does not exist. \
                 Check the path and try again."
            );
        }
        if unverified_allowed() {
            return accept_unverified(&path);
        }
        let Some(bundle) = bundle else {
            panic!(
                "pdfium-bundled[bundled]: no pinned digest for target {target_os}/{target_arch}, so \
                 PDFIUM_BUNDLE_LIB={p} cannot be verified. Set {ALLOW_UNVERIFIED_ENV}=1 to embed it anyway."
            );
        };
        if let Err(mismatch) = check_file("library", bundle.lib_sha256, &path) {
            panic!(
                "pdfium-bundled[bundled]: refusing PDFIUM_BUNDLE_LIB={p}: {mismatch}.\n\
                 The pinned digest is for chromium/{PDFIUM_VERSION} {}; supply that build, or set \
                 {ALLOW_UNVERIFIED_ENV}=1 to embed a library this crate has not vouched for.",
                bundle.lib_name
            );
        }
        println!("cargo:warning=pdfium-bundled[bundled]: using PDFIUM_BUNDLE_LIB={p} (digest verified)");
        return path;
    }

    let bundle = bundle.unwrap_or_else(|| {
        panic!(
            "pdfium-bundled[bundled]: unsupported target {target_os}/{target_arch}.\n\
             Supported: macos/aarch64|x86_64, linux/x86_64|aarch64,\n\
             windows/x86_64|aarch64|x86.\n\
             Set PDFIUM_BUNDLE_LIB=/path/to/libpdfium (and {ALLOW_UNVERIFIED_ENV}=1) to provide a custom library."
        )
    });

    let cache_dir = build_cache_dir(target_os, target_arch);
    let cached_lib = cache_dir.join(bundle.lib_name);

    if cached_lib.exists() {
        match check_file("library", bundle.lib_sha256, &cached_lib) {
            Ok(()) => {
                println!(
                    "cargo:warning=pdfium-bundled[bundled]: cache hit — {} for {target_os}/{target_arch} (digest \
                     verified)",
                    bundle.lib_name
                );
                return cached_lib;
            }
            Err(mismatch) => {
                // The cache is ours to manage: discard the bad copy and fetch
                // a fresh one rather than trusting it or giving up.
                println!(
                    "cargo:warning=pdfium-bundled[bundled]: discarding cached {}: {mismatch}; re-downloading",
                    cached_lib.display()
                );
                std::fs::remove_file(&cached_lib)
                    .unwrap_or_else(|e| panic!("pdfium-bundled: cannot remove {}: {e}", cached_lib.display()));
            }
        }
    }

    // Cache miss: download + verify + extract + verify
    std::fs::create_dir_all(&cache_dir).unwrap_or_else(|e| {
        panic!(
            "pdfium-bundled: failed to create cache dir {}: {e}",
            cache_dir.display()
        )
    });

    let url = format!("{BASE_URL}/chromium%2F{PDFIUM_VERSION}/{}", bundle.archive_name);
    let tgz_path = cache_dir.join(bundle.archive_name);

    download_file(&url, &tgz_path);
    if let Err(mismatch) = check_file("archive", bundle.archive_sha256, &tgz_path) {
        let _ = std::fs::remove_file(&tgz_path);
        panic!(
            "pdfium-bundled[bundled]: refusing {url}: {mismatch}.\n\
             The release asset no longer matches the digest pinned in src/platform.rs. Do not work around \
             this: confirm upstream's attestation (`just pin-digests {PDFIUM_VERSION}`) before changing the pin."
        );
    }
    extract_lib(&tgz_path, bundle.lib_path_in_archive, &cached_lib);

    // Remove the compressed archive — the extracted lib stays in the cache.
    let _ = std::fs::remove_file(&tgz_path);

    // The archive matched, so a library mismatch here means the table itself
    // is wrong (or extraction is): a bug to fix, never a build to ship.
    if let Err(mismatch) = check_file("library", bundle.lib_sha256, &cached_lib) {
        let _ = std::fs::remove_file(&cached_lib);
        panic!(
            "pdfium-bundled[bundled]: {mismatch} for {} extracted from a verified {}; the lib_sha256 pin in \
             src/platform.rs is inconsistent with its archive_sha256",
            bundle.lib_name, bundle.archive_name
        );
    }

    println!(
        "cargo:warning=pdfium-bundled[bundled]: cached {} at {} (digest verified)",
        bundle.lib_name,
        cached_lib.display()
    );

    cached_lib
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Fail the build, rather than the first `bind()`, if the pinned pdfium build
/// ever drops below the one `pdfium-render` targets. Both consts live in
/// `src/platform.rs`; this runs on every build, bundled or not.
fn assert_pin_at_least_api_floor() {
    let parse = |name: &str, value: &str| -> u32 {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be a bare pdfium build number, got {value:?}"))
    };
    let pinned = parse("PDFIUM_VERSION", PDFIUM_VERSION);
    let floor = parse("PDFIUM_API_FLOOR", PDFIUM_API_FLOOR);
    assert!(
        pinned >= floor,
        "PDFIUM_VERSION {pinned} is older than the {floor} build pdfium-render binds; bind() would fail with a missing symbol"
    );
}

fn main() {
    assert_pin_at_least_api_floor();

    println!("cargo:rerun-if-env-changed=PDFIUM_BUNDLE_LIB");
    println!("cargo:rerun-if-env-changed={ALLOW_UNVERIFIED_ENV}");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_BUNDLED");
    println!("cargo:rerun-if-env-changed=PDFIUM_BUILD_CACHE_DIR");
    println!("cargo:rerun-if-env-changed=DOCS_RS");

    if std::env::var("CARGO_FEATURE_BUNDLED").is_err() {
        return; // bundled feature not active — nothing to do
    }

    // docs.rs builds with `bundled` (see Cargo.toml docs.rs metadata) in a
    // network-isolated sandbox, so the curl download can't run. Emit an empty
    // stub so the bundled-only API still compiles and documents.
    if std::env::var("DOCS_RS").is_ok() {
        let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
        std::fs::write(out_dir.join("bundled.rs"), "pub static PDFIUM_BYTES: &[u8] = &[];\n")
            .unwrap_or_else(|e| panic!("pdfium-bundled: failed to write stub bundled.rs: {e}"));
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let lib_src = resolve_lib(&target_os, &target_arch);
    // A verified file can still be replaced on disk between builds (a new
    // PDFIUM_BUNDLE_LIB, a refreshed cache); re-run so the check re-runs.
    println!("cargo:rerun-if-changed={}", lib_src.display());

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));

    // Copy into OUT_DIR under a fixed, platform-neutral filename so the
    // include_bytes! path in the generated bundled.rs is always stable.
    let lib_dest = out_dir.join("bundled_pdfium_lib");
    std::fs::copy(&lib_src, &lib_dest).unwrap_or_else(|e| {
        panic!(
            "pdfium-bundled: failed to copy {} → {}: {e}",
            lib_src.display(),
            lib_dest.display()
        )
    });

    // Generate bundled.rs (include!()-ed by lib.rs).
    let bundled_rs = out_dir.join("bundled.rs");
    std::fs::write(
        &bundled_rs,
        "/// The pdfium shared library embedded at compile time.\n\
         ///\n\
         /// On first use, these bytes are extracted to the local cache\n\
         /// directory; see [`super::bind_bundled`].\n\
         pub static PDFIUM_BYTES: &[u8] = include_bytes!(\"bundled_pdfium_lib\");\n",
    )
    .unwrap_or_else(|e| panic!("pdfium-bundled: failed to write bundled.rs: {e}"));

    println!("cargo:rerun-if-changed={}", lib_dest.display());
}
