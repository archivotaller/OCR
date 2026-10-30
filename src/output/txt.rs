//! Renderizado de texto plano para las salidas TXT / Markdown.

use crate::ocr::engine::OcrLine;

/// Texto plano: una línea por región, en orden de lectura.
pub fn plain(lines: &[OcrLine]) -> String {
    lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Salida "markdown" simple: cabecera del archivo + texto.
pub fn markdown(file: &str, lines: &[OcrLine]) -> String {
    let mut out = String::new();
    if !file.is_empty() {
        out.push_str(&format!("# {file}\n\n"));
    }
    out.push_str(&plain(lines));
    if !lines.is_empty() {
        out.push('\n');
    }
    out
}
