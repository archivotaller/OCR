// SHA-256 verification shared by the library and the build script.
//
// Included two ways, exactly like `platform.rs`: `include!("src/integrity.rs")`
// in `build.rs` and `mod integrity;` in `src/lib.rs`. The same constraints
// follow — no `//!` header, nothing from `lib.rs` — and `sha2` is therefore
// both a dependency and a build-dependency.
//
// The rule these helpers enforce: every pdfium library this crate embeds or
// loads matches a digest pinned in `platform.rs`, and every archive it unpacks
// matches its pinned digest before the tar parser sees a byte. The digests are
// the crate's link to upstream's SLSA provenance (see `PlatformInfo`), so a
// mutated release asset, a poisoned cache, or a wrong `PDFIUM_BUNDLE_LIB` all
// fail closed instead of becoming native code in the caller's process.

use sha2::{Digest, Sha256};

/// Environment variable that disables digest verification of an explicitly
/// supplied library (`PDFIUM_BUNDLE_LIB` at build time, `PDFIUM_LIB_PATH` or a
/// [`bind_pdfium_from_path`](crate::bind_pdfium_from_path) argument at run
/// time). For people who build their own pdfium. Every acceptance is logged;
/// the downloaded and cached copies are never exempt.
pub const ALLOW_UNVERIFIED_ENV: &str = "PDFIUM_ALLOW_UNVERIFIED_LIB";

/// A file whose SHA-256 differs from the digest pinned in `platform.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigestMismatch {
    /// What was checked: `"archive"` or `"library"`.
    pub(crate) what: &'static str,
    /// The pinned digest, lowercase hex.
    pub(crate) expected: String,
    /// The digest of the bytes actually seen, lowercase hex.
    pub(crate) actual: String,
}

impl std::fmt::Display for DigestMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pdfium {} SHA-256 mismatch: expected {}, got {}",
            self.what, self.expected, self.actual
        )
    }
}

/// Lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

/// Checks `bytes` against the pinned `expected` digest.
pub(crate) fn verify_digest(what: &'static str, expected: &str, bytes: &[u8]) -> Result<(), DigestMismatch> {
    let actual = sha256_hex(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(DigestMismatch {
            what,
            expected: expected.to_string(),
            actual,
        })
    }
}

/// Whether the caller has opted out of verifying an explicitly supplied
/// library by setting [`ALLOW_UNVERIFIED_ENV`] to a non-empty value.
pub(crate) fn unverified_allowed() -> bool {
    std::env::var_os(ALLOW_UNVERIFIED_ENV).is_some_and(|value| !value.is_empty())
}
