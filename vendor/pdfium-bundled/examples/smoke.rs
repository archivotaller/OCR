//! End-to-end smoke test: bind PDFium and prove the FFI actually works.
//!
//! This is the harness CI runs to catch PDFIUM_VERSION / pdfium-render drift —
//! a failure that download + extract + `cargo build` all pass but a real
//! `bind()` does not (missing symbol at load time). See the crate docs and
//! `PDFIUM_VERSION` for why the pinned build must be at least `pdfium_latest`.
//!
//! Two library modes, both exercised by CI:
//!   - default features: `cargo run --example smoke` (runtime download)
//!   - `bundled` feature: `cargo run --example smoke --features bundled`

fn main() -> Result<(), pdfium_bundled::Error> {
    // Pick the binding that matches how the crate was compiled: the embedded
    // bytes when `bundled` is on, otherwise the runtime download + cache path.
    #[cfg(feature = "bundled")]
    let pdfium = pdfium_bundled::bind_bundled()?;
    #[cfg(not(feature = "bundled"))]
    let pdfium = pdfium_bundled::bind_pdfium_silent()?;

    // Binding alone resolves every symbol (which is what catches drift), but do
    // one real FFI round-trip so this is unambiguously an end-to-end check.
    let doc = pdfium.create_new_pdf().expect("create an empty PDF");
    println!(
        "bound pdfium {} — empty document has {} page(s)",
        pdfium_bundled::PDFIUM_VERSION,
        doc.pages().len()
    );

    Ok(())
}
