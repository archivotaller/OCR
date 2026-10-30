//! Salida estructurada JSON con cajas, texto y confianzas.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LineJson {
    /// Cuadrilátero (tl, tr, br, bl) en píxeles de la imagen original.
    pub points: [[f32; 2]; 4],
    pub text: String,
    pub det_score: f32,
    pub rec_score: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileJson {
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub elapsed_ms: u128,
    pub lines: Vec<LineJson>,
}

impl FileJson {
    pub fn from_result(r: &crate::output::common::FileResult) -> Self {
        FileJson {
            file: r.file.clone(),
            width: r.width,
            height: r.height,
            elapsed_ms: r.elapsed_ms,
            lines: r
                .lines
                .iter()
                .map(|l| LineJson {
                    points: l.points.map(|(x, y)| [x, y]),
                    text: l.text.clone(),
                    det_score: l.det_score,
                    rec_score: l.rec_score,
                })
                .collect(),
        }
    }
}
