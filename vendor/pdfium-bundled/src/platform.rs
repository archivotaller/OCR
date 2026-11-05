// Shared platform + version data for the library and the build script.
//
// `build.rs` is compiled before the crate it builds, so it can't `use` any of
// this crate's items. `include!("src/platform.rs")` textually pastes this file
// into both `build.rs` and `src/lib.rs` (the latter via `mod platform;`),
// making it the single source of truth for the pinned pdfium build and the
// per-platform archive layout — the two facts that previously lived, verbatim
// and un-enforced, in both files.
//
// Constraints that follow from being included two ways:
//   - No `//!` header (illegal when pasted mid-`build.rs`); use `//` and `///`.
//   - Nothing from `lib.rs` (the `Error` enum, its imports). `platform_for`
//     returns `Option` so each caller builds its own error type.

/// The `bblanchon/pdfium-binaries` release tag to download
/// ([`chromium/8066`](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium%2F8066)).
///
/// Must be at least [`PDFIUM_API_FLOOR`], the build that `pdfium-render`'s
/// `pdfium_latest` feature binds. Newer builds work because pdfium's C API only
/// grows: `pdfium-render` resolves every symbol eagerly at `bind()`, so a build
/// that dropped one would fail there with a missing-symbol error, not at
/// compile time. An older build fails the same way. `just smoke` and the CI
/// `bind` job exercise a real `bind()` on every change for exactly this
/// reason. Defined here once so the runtime download and the compile-time
/// embed can never disagree.
pub const PDFIUM_VERSION: &str = "8066";

/// The pdfium build `pdfium-render`'s `pdfium_latest` feature binds against
/// (`pdfium_latest = ["pdfium_7881"]` in pdfium-render 0.9.x). Update this when
/// bumping `pdfium-render` moves `pdfium_latest`; [`PDFIUM_VERSION`] must never
/// fall below it.
pub const PDFIUM_API_FLOOR: &str = "7881";

/// Base URL for `bblanchon/pdfium-binaries` release assets.
pub(crate) const BASE_URL: &str = "https://github.com/bblanchon/pdfium-binaries/releases/download";

/// Where to find the pdfium shared library for one target platform, and what
/// its bytes must hash to.
///
/// The two digests pin the [`PDFIUM_VERSION`] release: `archive_sha256` is the
/// `.tgz` exactly as published — the same value upstream signs as a subject in
/// its SLSA provenance attestation — and `lib_sha256` is the shared library
/// inside it, the bytes that actually get embedded or `dlopen()`ed. Every
/// download, cache hit, embed, and bind checks against them (see
/// `integrity.rs`), so a mutated release asset or a poisoned cache fails
/// closed.
///
/// Regenerate the table when bumping the pin with `just pin-digests <BUILD>`:
/// it downloads the seven archives, verifies each against upstream's
/// attestation with `gh attestation verify --repo bblanchon/pdfium-binaries
/// --signer-workflow
/// bblanchon/pdfium-binaries/.github/workflows/build-all.yml`, and prints the
/// digests ready to paste. Digests must be lowercase hex.
pub(crate) struct PlatformInfo {
    /// Asset filename in the GitHub release, e.g. `pdfium-mac-arm64.tgz`.
    pub(crate) archive_name: &'static str,
    /// Relative path inside the archive, e.g. `lib/libpdfium.dylib`.
    pub(crate) lib_path_in_archive: &'static str,
    /// Filename to write on disk, e.g. `libpdfium.dylib`.
    pub(crate) lib_name: &'static str,
    /// SHA-256 of the release archive as published; checked before extraction.
    pub(crate) archive_sha256: &'static str,
    /// SHA-256 of `lib_path_in_archive` once extracted; checked at every embed
    /// and every bind.
    pub(crate) lib_sha256: &'static str,
}

/// Maps a Rust `(os, arch)` pair — `std::env::consts` values at runtime, or the
/// `CARGO_CFG_TARGET_*` build vars at compile time — to its pdfium archive, or
/// `None` when the platform is unsupported. The caller owns the failure: it
/// already holds the pair and builds whatever error it needs.
pub(crate) fn platform_for(os: &str, arch: &str) -> Option<PlatformInfo> {
    let info = match (os, arch) {
        ("macos", "aarch64") => PlatformInfo {
            archive_name: "pdfium-mac-arm64.tgz",
            lib_path_in_archive: "lib/libpdfium.dylib",
            lib_name: "libpdfium.dylib",
            archive_sha256: "336219e80580b93c6523f44db7dc1de59cc497b13a7390ddac84223f68ca162b",
            lib_sha256: "1c5326c2ebf250026715c696ca7a391955028cf34312170ed9cf4d904382e59b",
        },
        ("macos", "x86_64") => PlatformInfo {
            archive_name: "pdfium-mac-x64.tgz",
            lib_path_in_archive: "lib/libpdfium.dylib",
            lib_name: "libpdfium.dylib",
            archive_sha256: "841ecac278cdd46288dd065873522cf72f3996560d8978f473d336f01d59942c",
            lib_sha256: "3653e54d1348cd6024e6d8031f31c0fb33885ad80d4ae9fdf3447a4b366f5a1e",
        },
        ("linux", "x86_64") => PlatformInfo {
            archive_name: "pdfium-linux-x64.tgz",
            lib_path_in_archive: "lib/libpdfium.so",
            lib_name: "libpdfium.so",
            archive_sha256: "0b43f405477cf2cfc4dbff06905093c3309756c6bca1fb9da99234a2ca97fed2",
            lib_sha256: "7670b3c597b02dfa3f98b23b49c3bb52536312f1ea686b739321731b6011f5a9",
        },
        ("linux", "aarch64") => PlatformInfo {
            archive_name: "pdfium-linux-arm64.tgz",
            lib_path_in_archive: "lib/libpdfium.so",
            lib_name: "libpdfium.so",
            archive_sha256: "0e6f90dccbc6b81fd5d7106abaf164c4222178f024c204d00d526b60fd2ad535",
            lib_sha256: "9d00f11a0b27d6860aadd6301c4d18c03d455a2d149f0fbef9dccdb9fb2f19f5",
        },
        ("windows", "x86_64") => PlatformInfo {
            archive_name: "pdfium-win-x64.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
            archive_sha256: "739a57d597d864297909cc40a2411eba728490c76a0fa25e3ea299c7f6b07020",
            lib_sha256: "d42c452a4cf8ca19a87e9c659d4e05035be742c21696ac13431cf73ac1bbf14b",
        },
        ("windows", "aarch64") => PlatformInfo {
            archive_name: "pdfium-win-arm64.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
            archive_sha256: "5d04b6d0281e78613ef836dea2e0fefe6831f3ae92b3573e8fdf55330de67d3d",
            lib_sha256: "6742bf441a0333fa5a796abe9f8ce0b23e51400a826255947f279038dfabe7d2",
        },
        ("windows", "x86") => PlatformInfo {
            archive_name: "pdfium-win-x86.tgz",
            lib_path_in_archive: "bin/pdfium.dll",
            lib_name: "pdfium.dll",
            archive_sha256: "83cc422d75cf853b3fd3b94bbf95d1d59b541d73c7a85b8385836810fe96a5df",
            lib_sha256: "9034c03e9481701da0aa058a6044344c330e105b7032d04fec0f8ca585431ca4",
        },
        _ => return None,
    };
    Some(info)
}
