//! Tipos comunes de salida.

use crate::ocr::engine::OcrLine;

/// Resultado de OCR de un archivo (posiblemente multipágina: una entrada por frame).
#[derive(Debug, Clone)]
pub struct FileResult {
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub elapsed_ms: u128,
    pub lines: Vec<OcrLine>,
}
