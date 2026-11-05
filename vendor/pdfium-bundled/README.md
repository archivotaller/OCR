# pdfium-bundled

Auto-download and cache [PDFium](https://pdfium.googlesource.com/pdfium/) binaries for
[`pdfium-render`](https://docs.rs/pdfium-render), so you never have to hand-download
`libpdfium` or fiddle with `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` again.

A forward-moving fork of
[`pdfium-auto`](https://github.com/raphaelmansuy/edgequake-pdf2md/tree/main/crates/pdfium-auto).
Because I am lazy and you are too.

## How it works

On the first call to [`bind_pdfium`] / [`ensure_pdfium_library`], the crate:

1. Checks the local cache for the platform library.
2. If it's missing, downloads the matching `.tgz` from
   [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) and extracts it.
3. Checks the archive and the extracted library against the SHA-256 digests pinned in
   the crate (see [Integrity](#integrity)).
4. Binds the library and hands you a ready-to-use `Pdfium`.

Every later call skips the network — the library is already cached (and re-checked).

## Install

```sh
cargo add pdfium-bundled
```

You do **not** need to depend on `pdfium-render` yourself: it's re-exported as
`pdfium_bundled::pdfium_render`, already at the matching version.

## Quick start

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Downloads + caches PDFium on first run, then binds it.
    let pdfium = pdfium_bundled::bind_pdfium_silent()?;

    let document = pdfium.load_pdf_from_file("example.pdf", None)?;
    println!("Loaded {} pages", document.pages().len());
    Ok(())
}
```

Want a progress bar for that first download? Pass a callback:

```rust
let pdfium = pdfium_bundled::bind_pdfium(Some(&|downloaded, total| {
    if let Some(total) = total {
        eprint!("\rDownloading PDFium: {downloaded}/{total} bytes");
    }
}))?;
```

## Compile-time bundling

For a fully self-contained binary (air-gapped runtimes, single-file CLI distribution),
enable the `bundled` feature to embed the library into the executable at build time:

```sh
cargo add pdfium-bundled --features bundled
```

The build script downloads and embeds the platform library (or uses `PDFIUM_BUNDLE_LIB`
if you set it). At runtime, call [`bind_bundled`] instead of `bind_pdfium` — no network
access is ever required.

|                             | Runtime download (default) | `bundled` feature   |
| --------------------------- | -------------------------- | ------------------- |
| Binary size                 | small                      | +~30 MB (embedded)  |
| First run                   | downloads once             | instant             |
| Network needed at runtime   | once                       | never               |
| Network needed at build     | never                      | once                |

## Integrity

The library this crate loads is native code running in your process, so nothing is
trusted on the strength of a URL. `src/platform.rs` pins, per platform, the SHA-256 of
the pinned release archive and of the shared library inside it. The archive digest is
the same value upstream signs as a subject of its SLSA provenance attestation, which is
how the table is regenerated on a bump:

```sh
just pin-digests 8066   # download, `gh attestation verify`, extract, print the table
```

The pins are enforced on every download (before the archive is unpacked), on every
cached copy at build time and at run time (a stale entry is discarded and refetched, or
rewritten from the embedded bytes), and on every bind — [`bind_pdfium_from_path`] hashes
the file before `dlopen`, whichever path produced it. A mismatch is
`Error::DigestMismatch` naming the expected and actual digests; under `bundled` it is a
build failure. Nothing is loaded.

The one exemption is a library you supplied yourself (`PDFIUM_BUNDLE_LIB`,
`PDFIUM_LIB_PATH`, or a `bind_pdfium_from_path` argument) with
`PDFIUM_ALLOW_UNVERIFIED_LIB` set to a non-empty value, for people building their own
pdfium. Each acceptance is logged. Downloaded and cached copies are never exempt, and a
binary built with the opt-out needs it at run time too.

## Configuration

All optional, all read from the environment:

| Variable                     | Effect                                                                      |
| ---------------------------- | --------------------------------------------------------------------------- |
| `PDFIUM_LIB_PATH`            | Use an existing `libpdfium` at this path; skip the download (still verified at bind). |
| `PDFIUM_BUNDLED_CACHE_DIR`   | Override the cache directory (see below).                                   |
| `PDFIUM_NO_AUTO_DOWNLOAD`    | Never hit the network; error unless the library is already cached (for CI). |
| `PDFIUM_ALLOW_UNVERIFIED_LIB`| Accept an explicitly supplied library whose digest does not match the pin. See [Integrity](#integrity). |
| `PDFIUM_BUNDLE_LIB`          | *(build time, `bundled`)* Path to the library to embed (verified against the pin). |
| `PDFIUM_BUILD_CACHE_DIR`     | *(build time, `bundled`)* Override the build-time download cache.           |

### Cache location

The runtime cache follows the [XDG base-directory
spec](https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html) on
**every** platform — macOS and Windows included — so it honors `$XDG_CACHE_HOME` and
otherwise lives at:

```
~/.cache/pdfium-bundled/pdfium-<VERSION>/
```

(That's `~/.cache`, not `~/Library/Caches` or `%LOCALAPPDATA%` — usually what you want for a
developer tool. Override it with `PDFIUM_BUNDLED_CACHE_DIR`.)

## Platform support

| OS      | Architectures            | Library          |
| ------- | ------------------------ | ---------------- |
| macOS   | `arm64`, `x86_64`        | `libpdfium.dylib`|
| Linux   | `x86_64`, `aarch64`      | `libpdfium.so`   |
| Windows | `x86_64`, `aarch64`, `x86` | `pdfium.dll`   |

The bundled PDFium build is pinned by [`PDFIUM_VERSION`], which is kept at or
above the build that `pdfium-render`'s `pdfium_latest` feature binds. Newer
builds are safe because pdfium's C API only grows; CI proves it with a real bind.

## Development

This repo uses [`just`](https://github.com/casey/just):

```sh
just setup   # install dev tools (cargo-nextest, …)
just test    # run the test suite via nextest
just ci      # what CI runs: tests, clippy, cargo audit, rustfmt check
just fmt     # format (nightly rustfmt)
```

Minimum supported Rust version: **1.88** (edition 2024).

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.

[`bind_pdfium`]: https://docs.rs/pdfium-bundled/latest/pdfium_bundled/fn.bind_pdfium.html
[`bind_bundled`]: https://docs.rs/pdfium-bundled/latest/pdfium_bundled/fn.bind_bundled.html
[`bind_pdfium_from_path`]: https://docs.rs/pdfium-bundled/latest/pdfium_bundled/fn.bind_pdfium_from_path.html
[`ensure_pdfium_library`]: https://docs.rs/pdfium-bundled/latest/pdfium_bundled/fn.ensure_pdfium_library.html
[`PDFIUM_VERSION`]: https://docs.rs/pdfium-bundled/latest/pdfium_bundled/constant.PDFIUM_VERSION.html
