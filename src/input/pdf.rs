//! Entrada PDF: rasterizado con PDFium (embebido) y metadatos de la fuente de
//! origen por línea.

use crate::geom::{quad_bounds, Affine};
use crate::ocr::engine::OcrLine;
use anyhow::{anyhow, Result};
use image::DynamicImage;
use pdfium_bundled::pdfium_render::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Raster de una página y la transformación a coordenadas de página.
pub struct PageRaster {
    pub image: DynamicImage,
    /// Píxeles de la imagen -> puntos de la página.
    pub to_page: Affine,
    pub width_pt: f32,
    pub height_pt: f32,
    /// Rotación intrínseca del PDF (0/90/180/270).
    pub rotation: u16,
    /// Página sin texto ni vectores: solo una imagen (escaneo).
    pub scanned: bool,
}

/// Fuente detectada en el PDF de origen para una línea de OCR.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceFont {
    pub family: String,
    pub size_pt: f32,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub mono: bool,
    pub embedded: bool,
    /// Caja real del texto en puntos de página `(x0, y0, x1, y1)`.
    pub bbox: Option<(f32, f32, f32, f32)>,
    /// Origen (línea base) del primer carácter, en puntos de página.
    pub baseline: Option<(f32, f32)>,
}

impl SourceFont {
    /// Descripción corta para el informe y la GUI.
    pub fn describe(&self) -> String {
        let mut style = String::new();
        if self.bold {
            style.push_str(" Bold");
        }
        if self.italic {
            style.push_str(" Italic");
        }
        format!("{}{} · {:.1} pt", self.family, style, self.size_pt)
    }
}

/// Documento PDF de entrada. PDFium se enlaza una vez y el documento se abre
/// bajo demanda (evita auto-referencias y mantiene el tipo simple).
pub struct PdfInput {
    pdfium: Pdfium,
    path: PathBuf,
    password: Option<String>,
}

impl PdfInput {
    pub fn open(path: &Path, password: Option<&str>) -> Result<Self> {
        let pdfium = pdfium_bundled::bind_bundled()
            .map_err(|e| anyhow!("no se pudo cargar PDFium embebido: {e}"))?;
        Ok(PdfInput {
            pdfium,
            path: path.to_path_buf(),
            password: password.map(|s| s.to_string()),
        })
    }

    fn with_doc<R>(&self, f: impl FnOnce(&PdfDocument<'_>) -> Result<R>) -> Result<R> {
        let doc = self
            .pdfium
            .load_pdf_from_file(&self.path, self.password.as_deref())
            .map_err(|e| anyhow!("abriendo {}: {e}", self.path.display()))?;
        f(&doc)
    }

    pub fn page_count(&self) -> Result<usize> {
        self.with_doc(|doc| Ok(doc.pages().len() as usize))
    }

    /// Rasteriza una página a `dpi` y calcula su transformación a puntos.
    pub fn rasterize(&self, page_index: usize, dpi: f32) -> Result<PageRaster> {
        self.with_doc(|doc| {
            let page = doc
                .pages()
                .get(page_index as PdfPageIndex)
                .map_err(|e| anyhow!("página {}: {e}", page_index + 1))?;

            let width_pt = page.width().value;
            let height_pt = page.height().value;
            let target_w = ((width_pt * dpi / 72.0).round() as i32).clamp(1, 20_000);
            let cfg = PdfRenderConfig::new().set_target_width(target_w);

            let bitmap = page
                .render_with_config(&cfg)
                .map_err(|e| anyhow!("rasterizando la página {}: {e}", page_index + 1))?;
            let image = bitmap
                .as_image()
                .map_err(|e| anyhow!("convirtiendo la página a imagen: {e}"))?
                .to_rgb8();

            let (x0, y0) = page
                .pixels_to_points(0, 0, &cfg)
                .map_err(|e| anyhow!("coordenadas de página: {e}"))?;
            let (x1, y1) = page
                .pixels_to_points(1, 0, &cfg)
                .map_err(|e| anyhow!("coordenadas de página: {e}"))?;
            let (x2, y2) = page
                .pixels_to_points(0, 1, &cfg)
                .map_err(|e| anyhow!("coordenadas de página: {e}"))?;
            let to_page = Affine {
                a: x1.value - x0.value,
                b: y1.value - y0.value,
                c: x2.value - x0.value,
                d: y2.value - y0.value,
                e: x0.value,
                f: y0.value,
            };

            let rotation = match page.rotation().unwrap_or(PdfPageRenderRotation::None) {
                PdfPageRenderRotation::None => 0,
                PdfPageRenderRotation::Degrees90 => 90,
                PdfPageRenderRotation::Degrees180 => 180,
                PdfPageRenderRotation::Degrees270 => 270,
            };

            let mut images = 0usize;
            let mut paths = 0usize;
            for object in page.objects().iter() {
                if object.as_image_object().is_some() {
                    images += 1;
                } else if object.as_path_object().is_some() {
                    paths += 1;
                }
            }
            let has_text = page.text().map(|t| !t.is_empty()).unwrap_or(false);
            let scanned = !has_text && images >= 1 && paths == 0 && rotation == 0;

            Ok(PageRaster {
                image: DynamicImage::ImageRgb8(image),
                to_page,
                width_pt,
                height_pt,
                rotation,
                scanned,
            })
        })
    }

    /// Fuente de origen de cada línea de OCR, leída del texto real del PDF.
    ///
    /// Devuelve `None` en las líneas que no se pueden asociar a caracteres del
    /// PDF (por ejemplo en documentos escaneados).
    pub fn source_fonts(
        &self,
        page_index: usize,
        raster: &PageRaster,
        lines: &[OcrLine],
    ) -> Result<Vec<Option<SourceFont>>> {
        if lines.is_empty() {
            return Ok(Vec::new());
        }
        self.with_doc(|doc| {
            let page = doc
                .pages()
                .get(page_index as PdfPageIndex)
                .map_err(|e| anyhow!("página {}: {e}", page_index + 1))?;
            let text = match page.text() {
                Ok(t) => t,
                Err(_) => return Ok(vec![None; lines.len()]),
            };

            struct CharInfo {
                x: f32,
                y: f32,
                origin_x: f32,
                origin_y: f32,
                left: f32,
                top: f32,
                right: f32,
                bottom: f32,
                whitespace: bool,
                family: String,
                size: f32,
                bold: bool,
                italic: bool,
                serif: bool,
                mono: bool,
                embedded: bool,
            }

            let mut chars: Vec<CharInfo> = Vec::new();
            for ch in text.chars().iter() {
                let Ok(bounds) = ch.tight_bounds() else {
                    continue;
                };
                let Ok((origin_x, origin_y)) = ch.origin() else {
                    continue;
                };
                let embedded = ch
                    .text_object()
                    .ok()
                    .and_then(|o| o.font().is_embedded().ok())
                    .unwrap_or(false);
                chars.push(CharInfo {
                    x: (bounds.left().value + bounds.right().value) / 2.0,
                    y: (bounds.top().value + bounds.bottom().value) / 2.0,
                    origin_x: origin_x.value,
                    origin_y: origin_y.value,
                    left: bounds.left().value,
                    top: bounds.top().value,
                    right: bounds.right().value,
                    bottom: bounds.bottom().value,
                    whitespace: ch
                        .unicode_char()
                        .map(|c| c.is_whitespace())
                        .unwrap_or(false),
                    family: ch.font_name(),
                    size: ch.scaled_font_size().value,
                    bold: ch.font_is_bold_reenforced(),
                    italic: ch.font_is_italic(),
                    serif: ch.font_is_serif(),
                    mono: ch.font_is_fixed_pitch(),
                    embedded,
                });
            }
            if chars.is_empty() {
                return Ok(vec![None; lines.len()]);
            }

            let mut out = Vec::with_capacity(lines.len());
            for line in lines {
                let mut page_pts = [(0.0f32, 0.0f32); 4];
                for (i, (px, py)) in line.points.iter().enumerate() {
                    page_pts[i] = raster.to_page.map(*px, *py);
                }
                let xs: Vec<f32> = page_pts.iter().map(|p| p.0).collect();
                let ys: Vec<f32> = page_pts.iter().map(|p| p.1).collect();
                let x0 = xs.iter().copied().fold(f32::MAX, f32::min);
                let x1 = xs.iter().copied().fold(f32::MIN, f32::max);
                let y0 = ys.iter().copied().fold(f32::MAX, f32::min);
                let y1 = ys.iter().copied().fold(f32::MIN, f32::max);

                let inside: Vec<&CharInfo> = chars
                    .iter()
                    .filter(|c| c.x >= x0 && c.x <= x1 && c.y >= y0 && c.y <= y1)
                    .collect();
                if inside.is_empty() {
                    out.push(None);
                    continue;
                }

                let mut counts: HashMap<&str, usize> = HashMap::new();
                for c in &inside {
                    *counts.entry(c.family.as_str()).or_insert(0) += 1;
                }
                let family = counts
                    .into_iter()
                    .max_by_key(|(_, n)| *n)
                    .map(|(f, _)| f.to_string())
                    .unwrap_or_default();
                let sample = inside
                    .iter()
                    .find(|c| c.family == family)
                    .copied()
                    .unwrap_or(inside[0]);
                let size = inside.iter().map(|c| c.size).sum::<f32>() / inside.len() as f32;

                // Caja real de la línea: unión de los caracteres visibles (se
                // excluyen los espacios, que solo añaden avance). Los límites
                // de PDFium pueden venir en cualquier orden vertical, así que
                // se normalizan con min/max.
                let ink: Vec<&CharInfo> =
                    inside.iter().copied().filter(|c| !c.whitespace).collect();
                let bbox = if ink.is_empty() {
                    None
                } else {
                    let mut x0 = f32::MAX;
                    let mut y0 = f32::MAX;
                    let mut x1 = f32::MIN;
                    let mut y1 = f32::MIN;
                    for c in &ink {
                        x0 = x0.min(c.left.min(c.right));
                        x1 = x1.max(c.left.max(c.right));
                        y0 = y0.min(c.top.min(c.bottom));
                        y1 = y1.max(c.top.max(c.bottom));
                    }
                    Some((x0, y0, x1, y1))
                };

                // Línea base: origen del primer carácter visible y mediana de
                // las ordenadas (robusta frente a superíndices o acentos).
                let baseline = {
                    let mut origins: Vec<(f32, f32)> =
                        ink.iter().map(|c| (c.origin_x, c.origin_y)).collect();
                    if origins.is_empty() {
                        None
                    } else {
                        origins.sort_by(|a, b| {
                            a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        let x = origins[0].0;
                        Some((x, origins[origins.len() / 2].1))
                    }
                };

                out.push(Some(SourceFont {
                    family,
                    size_pt: size.max(1.0),
                    bold: sample.bold,
                    italic: sample.italic,
                    serif: sample.serif,
                    mono: sample.mono,
                    embedded: sample.embedded,
                    bbox,
                    baseline,
                }));
            }
            Ok(out)
        })
    }

    /// Rectángulo de la página en puntos (para informes).
    pub fn page_points(&self, page_index: usize) -> Result<(f32, f32)> {
        self.with_doc(|doc| {
            let page = doc
                .pages()
                .get(page_index as PdfPageIndex)
                .map_err(|e| anyhow!("página {}: {e}", page_index + 1))?;
            Ok((page.width().value, page.height().value))
        })
    }
}

/// Caja de una línea en puntos de página (útil para depuración).
pub fn line_page_bounds(raster: &PageRaster, line: &OcrLine) -> (f32, f32, f32, f32) {
    let mut q = [[0.0f32; 2]; 4];
    for (i, (x, y)) in line.points.iter().enumerate() {
        q[i] = {
            let (x, y) = raster.to_page.map(*x, *y);
            [x, y]
        };
    }
    quad_bounds(&q)
}

/// Comprueba que PDFium se puede cargar (test de humo).
pub fn pdfium_disponible() -> bool {
    pdfium_bundled::bind_bundled().is_ok()
}

/// Contexto legible para errores de apertura.
pub fn describe_path(path: &Path) -> String {
    path.display().to_string()
}
