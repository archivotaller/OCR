//! # pdfium-bundled
//!
//! Auto-download and cache [PDFium](https://pdfium.googlesource.com/pdfium/)
//! binaries at runtime, so that users of `pdfium-render` no longer need to
//! manually download libpdfium and set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`.
//!
//! ## How it works
//!
//! On first call to [`bind_pdfium`] or [`ensure_pdfium_library`]:
//!
//! 1. Checks `~/.cache/pdfium-bundled/pdfium-{VERSION}/` for the platform
//!    library.
//! 2. If absent, downloads the correct `.tgz` from [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries).
//! 3. Extracts `lib/libpdfium.dylib` (or `.so` / `.dll`) to the cache dir.
//! 4. Calls [`Pdfium::bind_to_library`] to load the real library.
//!
//! Subsequent calls skip the network entirely — the library is already cached.
//!
//! ## `bundled` feature — compile-time embedding
//!
//! For use-cases that require a fully self-contained binary (e.g., CI/CD
//! distribution), the optional `bundled` feature embeds the pdfium shared
//! library directly into the compiled executable.
//!
//! **Build steps:**
//!
//! ```sh
//! # 1. Download and extract the platform archive (example: macOS arm64).
//! curl -L https://github.com/bblanchon/pdfium-binaries/releases/download/ \
//!      chromium%2F8066/pdfium-mac-arm64.tgz | tar xz
//!
//! # 2. Build with the bundled feature, pointing PDFIUM_BUNDLE_LIB at the lib.
//! PDFIUM_BUNDLE_LIB=./lib/libpdfium.dylib \
//!   cargo build --release --features pdfium-bundled/bundled
//! ```
//!
//! At runtime, the embedded bytes are extracted to the cache directory on
//! first use ([`ensure_pdfium_bundled`] / [`bind_bundled`]).  The resulting
//! binary ships without any external dependency on libpdfium or network access.
//!
//! **Trade-offs:**
//!
//! | | Runtime-download (`bind_pdfium`) | Compile-time-bundled (`bind_bundled`) |
//! |--|--|--|
//! | Binary size | ~5 MB | ~35 MB (+30 MB) |
//! | First run | Downloads pdfium (~20 s) | Instant (already embedded) |
//! | Net access required at runtime | Once (first run) | Never |
//! | Net access required at compile time | No | No |
//! | Cross-platform binary | N/A (same arch) | Same constraints |
//!
//! ## Usage
//!
//! ```rust,no_run
//! use pdfium_bundled::{bind_pdfium_silent, bind_pdfium_from_path, ensure_pdfium_library};
//!
//! // Option A: convenient one-shot bind (silent, no progress)
//! let pdfium = bind_pdfium_silent().expect("PDFium unavailable");
//!
//! // Option B: download with progress, then bind
//! let path = ensure_pdfium_library(Some(&|downloaded, total| {
//!     if let Some(t) = total {
//!         eprint!("\rDownloading PDFium: {}/{} bytes", downloaded, t);
//!     }
//! })).expect("download failed");
//! let pdfium = bind_pdfium_from_path(&path).expect("bind failed");
//! ```
//!
//! ## Platform support
//!
//! | OS      | Arch    | Library               |
//! |---------|---------|-----------------------|
//! | macOS   | arm64   | `libpdfium.dylib`     |
//! | macOS   | x86_64  | `libpdfium.dylib`     |
//! | Linux   | x86_64  | `libpdfium.so`        |
//! | Linux   | aarch64 | `libpdfium.so`        |
//! | Windows | x86_64  | `pdfium.dll`          |
//! | Windows | aarch64 | `pdfium.dll`          |
//! | Windows | x86     | `pdfium.dll`          |
//!
//! ## Integrity
//!
//! Every library this crate loads is native code running in your process, so
//! nothing is trusted on the strength of a URL. `src/platform.rs` pins, per
//! platform, the SHA-256 of the [`PDFIUM_VERSION`] release archive and of the
//! shared library inside it. The archive digest is the same value upstream
//! signs as a subject of its SLSA provenance attestation (`gh attestation
//! verify --repo bblanchon/pdfium-binaries`), which is how the table is
//! regenerated on a bump. The pins are enforced:
//!
//! - on every download, before the archive is unpacked;
//! - on every cached copy, at build time and at run time — a cache entry that
//!   no longer matches is discarded and refetched, or rewritten from the
//!   embedded bytes;
//! - on every bind: [`bind_pdfium_from_path`] hashes the file before `dlopen`,
//!   whichever path produced it.
//!
//! A mismatch is [`Error::DigestMismatch`] (a build failure under `bundled`),
//! naming the expected and actual digests; nothing is loaded. The only
//! exemption is a library you supplied yourself — `PDFIUM_BUNDLE_LIB`,
//! `PDFIUM_LIB_PATH`, or a [`bind_pdfium_from_path`] argument — when
//! [`ALLOW_UNVERIFIED_ENV`] (`PDFIUM_ALLOW_UNVERIFIED_LIB`) is set to a
//! non-empty value, for people building their own pdfium. Each acceptance is
//! logged. Downloaded and cached copies are never exempt, and a binary built
//! with the opt-out needs it at run time too.
//!
//! ## Environment variable overrides
//!
//! - `PDFIUM_LIB_PATH` — path to an existing pdfium library; skips download
//!   (still verified at bind).
//! - `PDFIUM_BUNDLED_CACHE_DIR` — override the default (runtime) cache
//!   directory.
//! - `PDFIUM_NO_AUTO_DOWNLOAD` — never hit the network; error unless the
//!   library is already cached (for CI).
//! - `PDFIUM_ALLOW_UNVERIFIED_LIB` — accept an explicitly supplied library
//!   whose digest does not match the pin. See *Integrity*.
//! - `PDFIUM_BUNDLE_LIB` — (compile time) path to the dylib to embed when the
//!   `bundled` feature is active (verified against the pin).
//! - `PDFIUM_BUILD_CACHE_DIR` — (compile time) override the build-time download
//!   cache.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use etcetera::base_strategy::{BaseStrategy, Xdg};
/// Re-export of the [`pdfium-render`](https://docs.rs/pdfium-render) crate that
/// backs this one. Callers use the returned [`Pdfium`] — and the rest of the
/// PDF API — through this re-export, so they never have to add (and
/// version-match) a separate `pdfium-render` dependency:
///
/// ```no_run
/// use pdfium_bundled::pdfium_render::prelude::*;
/// ```
pub use pdfium_render;
use pdfium_render::prelude::Pdfium;
use thiserror::Error;

mod integrity;
mod platform;

pub use crate::integrity::ALLOW_UNVERIFIED_ENV;
use crate::integrity::{DigestMismatch, unverified_allowed, verify_digest};
use crate::platform::{BASE_URL, PlatformInfo, platform_for};
pub use crate::platform::{PDFIUM_API_FLOOR, PDFIUM_VERSION};

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors returned by pdfium-bundled operations.
#[derive(Error, Debug)]
pub enum Error {
    #[error("Unsupported platform: {os}/{arch}")]
    UnsupportedPlatform { os: String, arch: String },

    #[error("Cache directory error: {0}")]
    CacheDir(#[source] std::io::Error),

    #[error("Download failed: {0}")]
    Download(String),

    #[error("Archive extraction failed: {0}")]
    Extract(String),

    /// The file at `path` could not be read for verification.
    #[error("Failed to read '{path}': {reason}")]
    Read { path: PathBuf, reason: String },

    /// The file's SHA-256 does not match the digest pinned in `platform.rs`
    /// for this platform and [`PDFIUM_VERSION`]. Nothing was loaded. `what` is
    /// `"archive"` or `"library"`; see the crate docs on integrity.
    #[error("pdfium {what} '{path}' failed verification: expected SHA-256 {expected}, got {actual}")]
    DigestMismatch {
        what: &'static str,
        path: PathBuf,
        expected: String,
        actual: String,
    },

    /// `pdfium-render` loaded the file but could not resolve its symbols —
    /// most often a [`PDFIUM_VERSION`] / `pdfium_latest` mismatch, not a
    /// corrupt library.
    #[error("Failed to bind PDFium from '{path}': {reason}")]
    Bind { path: PathBuf, reason: String },
}

// ── Internal: platform metadata ──────────────────────────────────────────────

/// Resolves the current host's pdfium archive, mapping an unsupported platform
/// to [`Error::UnsupportedPlatform`]. The lookup table lives in `platform.rs`,
/// shared verbatim with `build.rs` so the two can't drift.
fn detect_platform() -> Result<PlatformInfo, Error> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    platform_for(os, arch).ok_or_else(|| Error::UnsupportedPlatform {
        os: os.to_string(),
        arch: arch.to_string(),
    })
}

// ── Cache directory resolution ───────────────────────────────────────────────

/// Returns the per-version cache directory for the PDFium library.
///
/// Uses the [XDG base-directory strategy][xdg] on **every** platform — macOS
/// and Windows included — so the cache honors `$XDG_CACHE_HOME` when set and
/// otherwise falls back to `~/.cache`, rather than `~/Library/Caches` (macOS)
/// or `%LOCALAPPDATA%` (Windows). This is the layout developers usually expect
/// from a cross-platform CLI tool. The resulting path is:
///
/// - `$XDG_CACHE_HOME/pdfium-bundled/pdfium-{VERSION}/`, or
/// - `~/.cache/pdfium-bundled/pdfium-{VERSION}/` when `XDG_CACHE_HOME` is
///   unset.
///
/// Override the whole path by setting `PDFIUM_BUNDLED_CACHE_DIR`.
///
/// [xdg]: https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html
#[must_use]
pub fn pdfium_cache_dir() -> PathBuf {
    let override_dir = std::env::var_os("PDFIUM_BUNDLED_CACHE_DIR").map(PathBuf::from);

    // Fall back to a temp dir only if the home directory can't be determined.
    let base = Xdg::new()
        .map(|xdg| xdg.cache_dir())
        .unwrap_or_else(|_| std::env::temp_dir());

    resolve_cache_dir(override_dir, base)
}

/// Assembles the per-version cache directory from an optional override root and
/// the default base cache directory.
///
/// Split out from [`pdfium_cache_dir`] as a pure function (no environment or
/// filesystem access) so the layout logic can be unit-tested without mutating
/// process-global state — `std::env::set_var` is `unsafe` under the 2024
/// edition precisely because it races with concurrent readers.
fn resolve_cache_dir(override_dir: Option<PathBuf>, base: PathBuf) -> PathBuf {
    let root = override_dir.unwrap_or_else(|| base.join("pdfium-bundled"));
    root.join(format!("pdfium-{PDFIUM_VERSION}"))
}

// ── Thread-safe singleton path cache ─────────────────────────────────────────

static RESOLVED_PATH: OnceLock<PathBuf> = OnceLock::new();

// ── Public API ───────────────────────────────────────────────────────────────

/// Returns `true` if the PDFium library is already cached on disk (no network
/// access needed on next call to [`ensure_pdfium_library`]).
///
/// Also returns `true` when `PDFIUM_LIB_PATH` points to an existing file.
#[must_use]
pub fn is_pdfium_cached() -> bool {
    cached_pdfium_path().is_some()
}

/// Returns the on-disk path to the PDFium library, or `None` if not cached.
#[must_use]
pub fn cached_pdfium_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PDFIUM_LIB_PATH") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
    }
    if let Ok(info) = detect_platform() {
        let p = pdfium_cache_dir().join(info.lib_name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// The SHA-256 (lowercase hex) this platform's pdfium library must hash to —
/// the pin every bind is checked against. Record it in a build manifest, or
/// compare it against the file you ship.
pub fn pinned_library_sha256() -> Result<&'static str, Error> {
    detect_platform().map(|info| info.lib_sha256)
}

/// Ensures the PDFium dynamic library is present in the local cache.
///
/// - If `PDFIUM_LIB_PATH` is set (and the file exists), that path is used.
/// - Otherwise, checks `pdfium_cache_dir()` for an existing library.
/// - If absent, downloads the appropriate platform binary from GitHub and
///   extracts it to the cache directory.
///
/// `on_progress` receives `(bytes_downloaded, total_size_option)` during
/// the download.  Pass `None` to suppress progress callbacks.
///
/// # Thread safety
///
/// Safe to call from multiple threads simultaneously; the download happens
/// only once per process lifetime.
pub fn ensure_pdfium_library(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<PathBuf, Error> {
    // Fast path: already resolved in this process.
    if let Some(path) = RESOLVED_PATH.get() {
        return Ok(path.clone());
    }

    let path = resolve_or_download(on_progress)?;

    // Best-effort cache in the OnceLock (ignore race; both will succeed).
    let _ = RESOLVED_PATH.set(path.clone());

    Ok(path)
}

/// Binds to PDFium, downloading it first if necessary.
///
/// `on_progress` receives `(bytes_downloaded, total_bytes_option)` during
/// the initial download.
pub fn bind_pdfium(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<Pdfium, Error> {
    let lib_path = ensure_pdfium_library(on_progress)?;
    bind_pdfium_from_path(&lib_path)
}

/// Binds to PDFium without any progress output.
///
/// Downloads and caches on first call if required.
pub fn bind_pdfium_silent() -> Result<Pdfium, Error> {
    bind_pdfium(None)
}

/// Binds to a PDFium library at an explicit `path`.
///
/// Does not interact with the download / cache layer, but does verify the
/// file: its SHA-256 must match this platform's pin (see the crate docs on
/// integrity) unless [`ALLOW_UNVERIFIED_ENV`] is set, in which case the
/// acceptance is logged. Every other bind in this crate ends here, so this is
/// the one place native code is checked before `dlopen`.
pub fn bind_pdfium_from_path(path: &Path) -> Result<Pdfium, Error> {
    verify_library_file(path)?;
    Pdfium::bind_to_library(path).map(Pdfium::new).map_err(|e| Error::Bind {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

// ── Bundled feature ──────────────────────────────────────────────────────────
//
// With `--features bundled`, build.rs embeds the pdfium bytes into the binary
// via `include_bytes!`; on first use they're written to the cache dir and
// loaded from there. The crate-level docs cover the build workflow.

#[cfg(feature = "bundled")]
mod bundled_lib {
    // `bundled.rs` is generated by build.rs and defines:
    //   pub static PDFIUM_BYTES: &[u8] = include_bytes!("bundled_pdfium_lib");
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

/// Ensures the embedded PDFium library is extracted to the local cache and
/// returns its on-disk path.
///
/// The bytes are embedded at compile time (via `PDFIUM_BUNDLE_LIB`); on first
/// call they are written to `pdfium_cache_dir()` so the OS can load them, and
/// later calls just return the cached path. A cache file that no longer
/// matches the pinned digest is rewritten from the embedded bytes.
///
/// # Errors
///
/// Returns [`Error::CacheDir`] if the cache directory cannot be created, or
/// [`Error::Extract`] if writing the library fails.
#[cfg(feature = "bundled")]
pub fn ensure_pdfium_bundled() -> Result<PathBuf, Error> {
    // Fast path: already resolved in this process.
    if let Some(path) = RESOLVED_PATH.get() {
        return Ok(path.clone());
    }

    let info = detect_platform()?;
    let cache_dir = pdfium_cache_dir();
    let lib_path = cache_dir.join(info.lib_name);

    // Under the opt-out the embedded bytes may legitimately not match the pin,
    // so only the verifying path treats a mismatching cache file as stale.
    let stale = lib_path.exists() && !unverified_allowed() && !cached_library_is_intact(&lib_path, &info);
    if stale {
        eprintln!(
            "pdfium-bundled: cached {} does not match the pinned digest; rewriting from the embedded library",
            lib_path.display()
        );
    }

    if stale || !lib_path.exists() {
        std::fs::create_dir_all(&cache_dir).map_err(Error::CacheDir)?;
        std::fs::write(&lib_path, bundled_lib::PDFIUM_BYTES).map_err(|e| {
            Error::Extract(format!(
                "Failed to write bundled pdfium to {}: {}",
                lib_path.display(),
                e
            ))
        })?;

        // On Unix, ensure the shared library is executable so the dynamic
        // linker accepts it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&lib_path).map_err(Error::CacheDir)?.permissions();
            perms.set_mode(perms.mode() | 0o755);
            std::fs::set_permissions(&lib_path, perms).map_err(Error::CacheDir)?;
        }
    }

    let _ = RESOLVED_PATH.set(lib_path.clone());
    Ok(lib_path)
}

/// Binds to the PDFium library that was embedded at compile time.
///
/// Extracts the library to the local cache directory on first call (see
/// [`ensure_pdfium_bundled`]). No network access is required.
#[cfg(feature = "bundled")]
pub fn bind_bundled() -> Result<Pdfium, Error> {
    let lib_path = ensure_pdfium_bundled()?;
    bind_pdfium_from_path(&lib_path)
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn read_file(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|e| Error::Read {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

fn mismatch_error(path: &Path, mismatch: DigestMismatch) -> Error {
    Error::DigestMismatch {
        what: mismatch.what,
        path: path.to_path_buf(),
        expected: mismatch.expected,
        actual: mismatch.actual,
    }
}

/// Reads `path` and checks it against this platform's pinned library digest —
/// the check every bind goes through. Honors the opt-out for libraries the
/// caller supplied, logging each acceptance.
fn verify_library_file(path: &Path) -> Result<(), Error> {
    if unverified_allowed() {
        eprintln!(
            "pdfium-bundled: {ALLOW_UNVERIFIED_ENV} is set — loading {} WITHOUT verifying its digest",
            path.display()
        );
        return Ok(());
    }
    let info = detect_platform()?;
    let bytes = read_file(path)?;
    verify_digest("library", info.lib_sha256, &bytes).map_err(|mismatch| mismatch_error(path, mismatch))
}

/// Checks an existing cache entry, never honoring the opt-out: the cache is
/// this crate's own, so nothing but the pinned bytes belongs in it. An
/// unreadable file counts as not intact.
fn cached_library_is_intact(path: &Path, info: &PlatformInfo) -> bool {
    matches!(std::fs::read(path), Ok(bytes) if verify_digest("library", info.lib_sha256, &bytes).is_ok())
}

fn resolve_or_download(on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<PathBuf, Error> {
    if let Ok(env_path) = std::env::var("PDFIUM_LIB_PATH") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            // Verified (or exempted) at bind time, like every other path.
            return Ok(p);
        }
        // Fall through: env var set but file missing → still auto-download.
        eprintln!(
            "pdfium-bundled: PDFIUM_LIB_PATH '{}' not found; downloading …",
            p.display()
        );
    }

    let info = detect_platform()?;
    let cache_dir = pdfium_cache_dir();
    let lib_path = cache_dir.join(info.lib_name);

    if lib_path.exists() {
        if cached_library_is_intact(&lib_path, &info) {
            return Ok(lib_path);
        }
        // Our cache, our rules: a copy that no longer hashes to the pin is
        // discarded and fetched again rather than trusted or tolerated.
        eprintln!(
            "pdfium-bundled: cached {} does not match the pinned digest; discarding it",
            lib_path.display()
        );
        std::fs::remove_file(&lib_path).map_err(Error::CacheDir)?;
    }

    // Opt-out for CI or test stages that must never touch the network. Checked
    // after the cache so an already-cached library still binds offline, as the
    // variable's documentation has always promised.
    if std::env::var("PDFIUM_NO_AUTO_DOWNLOAD").is_ok() {
        return Err(Error::Download(
            "auto-download disabled (PDFIUM_NO_AUTO_DOWNLOAD is set); \
             set PDFIUM_LIB_PATH to point at an existing pdfium library"
                .to_string(),
        ));
    }

    let url = format!("{}/chromium%2F{}/{}", BASE_URL, PDFIUM_VERSION, info.archive_name);

    std::fs::create_dir_all(&cache_dir).map_err(Error::CacheDir)?;

    let archive_bytes = download_bytes(&url, on_progress)?;
    // Before the tar parser sees a byte: the archive must be the one upstream
    // attested to when this pin was recorded.
    verify_digest("archive", info.archive_sha256, &archive_bytes)
        .map_err(|mismatch| mismatch_error(Path::new(info.archive_name), mismatch))?;
    extract_library(&archive_bytes, info.lib_path_in_archive, &lib_path)?;

    Ok(lib_path)
}

/// Streams a URL into a `Vec<u8>`, calling `on_progress` every 64 KiB.
fn download_bytes(url: &str, on_progress: Option<&dyn Fn(u64, Option<u64>)>) -> Result<Vec<u8>, Error> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent(concat!("pdfium-bundled/", env!("CARGO_PKG_VERSION")))
        .max_redirects(5)
        .build()
        .into();

    // ureq returns non-2xx as an error by default (http_status_as_error).
    let mut response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(code) => Error::Download(format!("HTTP {code} for {url}")),
        other => Error::Download(format!("GET {url}: {other}")),
    })?;

    let total = response.body().content_length();
    let capacity = total.unwrap_or(35 * 1024 * 1024) as usize;
    let mut buf = Vec::with_capacity(capacity);

    let mut reader = response.body_mut().as_reader();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut downloaded: u64 = 0;

    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                downloaded += n as u64;
                if let Some(cb) = on_progress {
                    cb(downloaded, total);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(Error::Download(format!("Read error: {e}")));
            }
        }
    }

    Ok(buf)
}

/// Extracts a single file from a gzipped tar archive into `dest_path`.
fn extract_library(archive_bytes: &[u8], lib_path_in_archive: &str, dest_path: &Path) -> Result<(), Error> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);

    for entry in archive.entries().map_err(|e| Error::Extract(e.to_string()))? {
        let mut entry = entry.map_err(|e| Error::Extract(e.to_string()))?;
        let entry_path = entry.path().map_err(|e| Error::Extract(e.to_string()))?;

        let entry_str = entry_path.to_string_lossy();
        if entry_str == lib_path_in_archive {
            entry
                .unpack(dest_path)
                .map_err(|e| Error::Extract(format!("Unpack failed: {e}")))?;
            return Ok(());
        }
    }

    Err(Error::Extract(format!(
        "Library '{}' not found in archive",
        lib_path_in_archive
    )))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_pdfium_is_at_least_the_api_floor() {
        let pinned: u32 = PDFIUM_VERSION.parse().expect("PDFIUM_VERSION is a bare build number");
        let floor: u32 = PDFIUM_API_FLOOR
            .parse()
            .expect("PDFIUM_API_FLOOR is a bare build number");
        assert!(
            pinned >= floor,
            "PDFIUM_VERSION {pinned} is older than the {floor} build pdfium-render binds; bind() would fail"
        );
    }

    #[test]
    fn detect_platform_is_supported() {
        detect_platform().expect("current platform should be supported");
    }

    #[test]
    fn cache_dir_is_deterministic() {
        // pdfium_cache_dir() only *reads* the environment, so it is stable
        // across calls within a process without any env mutation.
        let d1 = pdfium_cache_dir();
        let d2 = pdfium_cache_dir();
        assert_eq!(d1, d2);
        assert!(
            d1.to_str().expect("cache dir is valid UTF-8").contains(PDFIUM_VERSION),
            "expected PDFIUM_VERSION {PDFIUM_VERSION} in {d1:?}"
        );
    }

    #[test]
    fn cache_dir_default_layout() {
        let d = resolve_cache_dir(None, PathBuf::from("/tmp/example-cache"));
        assert_eq!(
            d,
            PathBuf::from(format!("/tmp/example-cache/pdfium-bundled/pdfium-{PDFIUM_VERSION}"))
        );
    }

    #[test]
    fn cache_dir_override_takes_precedence() {
        let d = resolve_cache_dir(
            Some(PathBuf::from("/tmp/custom-override")),
            PathBuf::from("/unused-base"),
        );
        assert_eq!(
            d,
            PathBuf::from(format!("/tmp/custom-override/pdfium-{PDFIUM_VERSION}"))
        );
        assert!(
            !d.to_str().expect("cache dir is valid UTF-8").contains("unused-base"),
            "override must ignore the default base: {d:?}"
        );
    }

    #[test]
    fn platform_info_fields_nonempty() {
        let info = detect_platform().expect("current platform should be supported");
        assert!(!info.archive_name.is_empty());
        assert!(!info.lib_path_in_archive.is_empty());
        assert!(!info.lib_name.is_empty());
    }

    #[test]
    fn platform_for_maps_a_known_target() {
        let linux = platform_for("linux", "x86_64").expect("linux/x86_64 is supported");
        assert_eq!(linux.archive_name, "pdfium-linux-x64.tgz");
        assert_eq!(linux.lib_path_in_archive, "lib/libpdfium.so");
        assert_eq!(linux.lib_name, "libpdfium.so");
    }

    #[test]
    fn platform_for_rejects_unknown_targets() {
        assert!(platform_for("freebsd", "x86_64").is_none());
        assert!(platform_for("linux", "riscv64").is_none());
    }

    // ─── Integrity ───────────────────────────────────────────────────────────

    use crate::integrity::sha256_hex;

    #[test]
    fn sha256_hex_matches_the_fips_180_2_vectors() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn verify_digest_reports_what_expected_and_actual() {
        let expected = sha256_hex(b"abc");
        assert_eq!(verify_digest("library", &expected, b"abc"), Ok(()));

        let mismatch = verify_digest("archive", &expected, b"abd").expect_err("different bytes must not verify");
        assert_eq!(mismatch.what, "archive");
        assert_eq!(mismatch.expected, expected);
        assert_eq!(mismatch.actual, sha256_hex(b"abd"));

        let text = mismatch.to_string();
        for needle in ["archive", &mismatch.expected, &mismatch.actual] {
            assert!(text.contains(needle), "{text:?} should name {needle}");
        }
    }

    #[test]
    fn every_platform_pins_distinct_lowercase_hex_digests() {
        let targets = [
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
            ("windows", "x86"),
        ];
        let mut seen = std::collections::HashSet::new();
        for (os, arch) in targets {
            let info = platform_for(os, arch).unwrap_or_else(|| panic!("{os}/{arch} is in the table"));
            for digest in [info.archive_sha256, info.lib_sha256] {
                assert_eq!(digest.len(), 64, "{os}/{arch}: {digest:?} is not a SHA-256");
                assert!(
                    digest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                    "{os}/{arch}: {digest:?} must be lowercase hex"
                );
                assert!(
                    seen.insert(digest),
                    "{os}/{arch}: {digest} is pinned twice — a copy-paste slip"
                );
            }
        }
    }

    #[test]
    fn pinned_library_sha256_is_this_platforms_pin() {
        let pin = pinned_library_sha256().expect("current platform should be supported");
        let info = detect_platform().expect("current platform should be supported");
        assert_eq!(pin, info.lib_sha256);
    }

    #[test]
    fn bind_refuses_a_file_that_does_not_match_the_pin() {
        // No env mutation: this exercises the default, verifying path. With
        // the opt-out exported in the developer's shell the assertion would be
        // meaningless, so skip rather than mis-report.
        if unverified_allowed() {
            eprintln!("skipping: {ALLOW_UNVERIFIED_ENV} is set");
            return;
        }
        let dir = std::env::temp_dir().join(format!("pdfium-bundled-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let bogus = dir.join("libpdfium-bogus");
        let contents = b"not a shared library";
        std::fs::write(&bogus, contents).expect("write bogus library");

        // `Pdfium` is not `Debug`, so `expect_err` is unavailable; match
        // instead.
        let err = match bind_pdfium_from_path(&bogus) {
            Ok(_) => panic!("a bogus library must not bind"),
            Err(err) => err,
        };
        let _ = std::fs::remove_dir_all(&dir);

        match err {
            Error::DigestMismatch {
                what,
                path,
                expected,
                actual,
            } => {
                assert_eq!(what, "library");
                assert_eq!(path, bogus);
                assert_eq!(expected, detect_platform().expect("supported").lib_sha256);
                assert_eq!(actual, sha256_hex(contents));
            }
            other => panic!("expected DigestMismatch before any dlopen, got {other:?}"),
        }
    }
}
