//! Librería `octxt`: motor OCR PP-OCRv6 (tract / 100% Rust) y salidas.
//!
//! Este crate es una lib + dos binarios:
//!
//! - `octxt`      : CLI (src/main.rs)
//! - `octxt-gui`  : GUI de escritorio (src/bin/octxt-gui/main.rs)
//!
//! Ambos comparten los mismos módulos de OCR, carga de imágenes, modelos y
//! generación de salidas (TXT / JSON / PDF buscable).

pub mod dict;
pub mod image_io;
pub mod models;
pub mod ocr;
pub mod output;
