//! Planificación del reemplazo de texto: qué se borra, con qué color y dónde se
//! dibuja el texto nuevo.
//!
//! El plan se calcula antes de tocar la imagen o el PDF, de modo que la GUI
//! pueda mostrar la vista previa y excluir cajas sin rehacer el OCR.

use crate::font::subset::SubsetFont;
use crate::font::{FontSet, FontSpec};
use crate::geom::{expand_quad, intersect, point_in_quad, quad_bounds, rect_quad, Affine, Quad};
use crate::input::pdf::SourceFont;
use crate::ocr::engine::OcrLine;
use crate::replace::color::{fill_quad, sample_background, sample_text_color, Rgb};
use anyhow::Result;
use image::{DynamicImage, RgbImage};
use std::fmt;

/// Cómo se elimina el texto original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    /// Se pintan los píxeles de la zona (imagen/TIFF y páginas escaneadas).
    Real,
    /// Se deja la imagen intacta y se tapa con un rectángulo opaco.
    Visual,
}

impl EraseMode {
    pub fn parse(s: &str) -> Option<EraseMode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "real" | "pixel" | "pixeles" => Some(EraseMode::Real),
            "visual" | "overlay" | "capa" => Some(EraseMode::Visual),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EraseMode::Real => "real",
            EraseMode::Visual => "visual",
        }
    }
}

/// Opciones del reemplazo.
#[derive(Debug, Clone)]
pub struct ReplaceOptions {
    pub mode: EraseMode,
    /// Margen alrededor de la caja, como fracción de su altura.
    pub margin: f32,
    /// Color de fondo fijo; `None` = automático por caja.
    pub background: Option<Rgb>,
    /// Color del texto fijo; `None` = automático por caja.
    pub text_color: Option<Rgb>,
    /// Fuentes del texto nuevo.
    pub font: FontSpec,
    /// Resolución de rasterizado de los PDF de entrada.
    pub dpi: f32,
    /// Cajas excluidas `(página 0-based, línea)`.
    pub excluded: Vec<(usize, usize)>,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        ReplaceOptions {
            mode: EraseMode::Real,
            margin: 0.3,
            background: None,
            text_color: None,
            font: FontSpec::default(),
            dpi: 300.0,
            excluded: Vec::new(),
        }
    }
}

/// Página ya rasterizada (imagen o PDF) con sus líneas OCR.
pub struct PageContent {
    pub image: DynamicImage,
    /// Píxeles del raster -> puntos de la página.
    pub to_page: Affine,
    pub width_pt: f32,
    pub height_pt: f32,
    /// Rotación intrínseca del PDF (0/90/180/270).
    pub rotation: u16,
    /// Página sin texto ni vectores: solo una imagen escaneada.
    pub scanned: bool,
    /// `true` si la página viene de un PDF existente.
    pub from_pdf: bool,
    /// Fuente de origen por línea (solo PDF con texto).
    pub source_fonts: Vec<Option<SourceFont>>,
    pub lines: Vec<OcrLine>,
}

impl PageContent {
    /// Página creada desde una imagen suelta: 1 píxel = 1 punto.
    pub fn from_image(image: DynamicImage) -> Self {
        let (w, h) = (image.width(), image.height());
        PageContent {
            image,
            to_page: Affine::image_to_page(w, h, w as f32, h as f32),
            width_pt: w as f32,
            height_pt: h as f32,
            rotation: 0,
            scanned: false,
            from_pdf: false,
            source_fonts: Vec::new(),
            lines: Vec::new(),
        }
    }

    /// `true` si hay que conservar el contenido original y superponer encima.
    /// Las páginas vectoriales de un PDF nunca se aplanan.
    pub fn overlay(&self) -> bool {
        self.from_pdf && !self.scanned
    }
}

/// Motivo por el que una línea no se reemplaza.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Empty,
    Excluded,
    Degenerate,
    MissingGlyphs(Vec<char>),
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkipReason::Empty => write!(f, "texto vacío"),
            SkipReason::Excluded => write!(f, "excluida por el usuario"),
            SkipReason::Degenerate => write!(f, "caja degenerada"),
            SkipReason::MissingGlyphs(chars) => {
                let list: String = chars.iter().collect();
                write!(f, "sin glifos para «{list}»")
            }
        }
    }
}

/// Fragmento de texto con una única fuente.
#[derive(Debug, Clone)]
pub struct TextRun {
    /// Índice en `Plan::subsets`.
    pub font: usize,
    /// Posición inicial en puntos de página.
    pub x: f32,
    /// Identificadores de glifo (CID) en orden.
    pub cids: Vec<u16>,
    /// Ancho total en puntos.
    pub advance: f32,
}

/// Línea que se va a borrar y reescribir.
#[derive(Debug, Clone)]
pub struct LinePlan {
    pub line: usize,
    pub text: String,
    /// Polígono de borrado en píxeles del raster.
    pub erase_px: Quad,
    /// Polígono de borrado en puntos de página.
    pub erase_pt: Quad,
    pub background: Rgb,
    pub uniform_background: bool,
    pub text_color: Rgb,
    pub size_pt: f32,
    pub baseline: (f32, f32),
    pub runs: Vec<TextRun>,
    pub bold: bool,
    pub italic: bool,
    pub source: Option<SourceFont>,
}

#[derive(Debug, Clone)]
pub struct SkippedLine {
    pub line: usize,
    pub text: String,
    pub reason: SkipReason,
}

/// Plan de una página.
#[derive(Debug, Clone)]
pub struct PagePlan {
    pub page: usize,
    /// `true`: se conserva el contenido original y se superpone el parche.
    pub overlay: bool,
    /// `true`: el fondo del documento no es uniforme en alguna caja.
    pub non_uniform: bool,
    pub lines: Vec<LinePlan>,
    pub skipped: Vec<SkippedLine>,
}

/// Plan completo del documento.
pub struct Plan {
    pub pages: Vec<PagePlan>,
    /// Subconjunto por índice de fuente (las no usadas quedan en `None`).
    pub subsets: Vec<Option<SubsetFont>>,
    pub font_labels: Vec<String>,
}

impl Plan {
    pub fn erased_lines(&self) -> usize {
        self.pages.iter().map(|p| p.lines.len()).sum()
    }

    pub fn skipped_lines(&self) -> usize {
        self.pages.iter().map(|p| p.skipped.len()).sum()
    }

    pub fn overlay_pages(&self) -> usize {
        self.pages.iter().filter(|p| p.overlay).count()
    }
}

/// Construye el plan de todas las páginas.
pub fn build(pages: &[PageContent], opts: &ReplaceOptions) -> Result<Plan> {
    let mut fonts = FontSet::load(&opts.font)?;
    let font_labels: Vec<String> = fonts.sources().iter().map(|s| s.label.clone()).collect();

    // 1. Filtrar líneas y comprobar cobertura de glifos.
    struct Candidate<'a> {
        page: usize,
        line: usize,
        text: &'a str,
        quad: Quad,
    }
    let mut candidates: Vec<Candidate<'_>> = Vec::new();
    let mut page_skips: Vec<Vec<SkippedLine>> = Vec::with_capacity(pages.len());

    for (page_idx, page) in pages.iter().enumerate() {
        let mut skips = Vec::new();
        for (line_idx, line) in page.lines.iter().enumerate() {
            let text = line.text.trim();
            let excluded = opts
                .excluded
                .iter()
                .any(|(p, l)| *p == page_idx && *l == line_idx);
            let mut quad = [[0.0f32; 2]; 4];
            for (i, (x, y)) in line.points.iter().enumerate() {
                quad[i] = [*x, *y];
            }
            let (x0, y0, x1, y1) = quad_bounds(&quad);
            let reason = if text.is_empty() {
                Some(SkipReason::Empty)
            } else if excluded {
                Some(SkipReason::Excluded)
            } else if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
                Some(SkipReason::Degenerate)
            } else if !fonts.can_render(text) {
                Some(SkipReason::MissingGlyphs(fonts.missing_chars(text)))
            } else {
                None
            };
            if let Some(reason) = reason {
                skips.push(SkippedLine {
                    line: line_idx,
                    text: line.text.clone(),
                    reason,
                });
            } else {
                candidates.push(Candidate {
                    page: page_idx,
                    line: line_idx,
                    text,
                    quad,
                });
            }
        }
        page_skips.push(skips);
    }

    // 2. Subconjuntos con todos los caracteres que se van a dibujar.
    let texts: Vec<&str> = candidates.iter().map(|c| c.text).collect();
    let subsets = fonts.build_subsets(&texts)?;

    // 3. Métricas y colores de cada línea.
    let mut page_lines: Vec<Vec<LinePlan>> = vec![Vec::new(); pages.len()];
    for cand in &candidates {
        let page = &pages[cand.page];
        let line = &page.lines[cand.line];
        let rgb = page.image.to_rgb8();

        let (x0, y0, x1, y1) = quad_bounds(&cand.quad);
        let height_px = (y1 - y0).max(1.0);
        let margin_px = (opts.margin.clamp(0.0, 1.0) * height_px).max(1.0);
        let band = (0.12 * height_px).max(2.0);

        // Fondo (override global o muestreo del anillo).
        let sample = sample_background(&rgb, &cand.quad, margin_px, band);
        let (background, uniform) = match opts.background {
            Some(c) => (c, true),
            None => (sample.color, sample.uniform()),
        };
        let text_color = opts
            .text_color
            .unwrap_or_else(|| sample_text_color(&rgb, &cand.quad, background));

        // Polígono de borrado en píxeles del OCR (para reconstrucción raster).
        let erase_px_ocr = expand_quad(&cand.quad, margin_px);
        if !point_in_quad((x0 + x1) / 2.0, (y0 + y1) / 2.0, &erase_px_ocr) {
            // El margen no debería romper el cuadrilátero; si lo hace, se usa
            // la caja original en lugar de arriesgar un borrado incorrecto.
            continue;
        }

        // Caja del OCR en puntos de página.
        let mut box_pt = [[0.0f32; 2]; 4];
        for (i, point) in box_pt.iter_mut().enumerate() {
            let (bx, by) = page.to_page.map(cand.quad[i][0], cand.quad[i][1]);
            *point = [bx, by];
        }
        let ocr_box = quad_bounds(&box_pt);

        // Caja real del texto y línea base, si el PDF de origen las aporta y
        // son coherentes con la caja del OCR. El detector ajusta a la altura
        // de x y deja fuera ascendentes/descendentes; con la caja real el
        // parche cubre toda la línea y el texto nuevo cae en su sitio.
        let source = page.source_fonts.get(cand.line).and_then(|s| s.as_ref());
        let real_box = source.and_then(|s| s.bbox).filter(|b| {
            let (rx0, ry0, rx1, ry1) = *b;
            rx1 > rx0 && ry1 > ry0 && intersect(*b, ocr_box).is_some()
        });

        // Rectángulo de borrado en puntos: caja real + margen o, si no hay
        // texto de origen, la caja del OCR transformada.
        let erase_pt = match real_box {
            Some((rx0, ry0, rx1, ry1)) => {
                let m = (opts.margin.clamp(0.0, 1.0) * (ry1 - ry0).max(1.0)).max(0.4);
                rect_quad(rx0 - m, ry0 - m, rx1 + m, ry1 + m)
            }
            None => {
                let mut q = [[0.0f32; 2]; 4];
                for i in 0..4 {
                    let (px, py) = page.to_page.map(erase_px_ocr[i][0], erase_px_ocr[i][1]);
                    q[i] = [px, py];
                }
                q
            }
        };

        // Para la vista previa, el parche se dibuja en píxeles: si hay caja
        // real se proyecta de vuelta al raster (cubre ascendentes y
        // descendentes); si no, se usa el polígono del OCR.
        let erase_px = if real_box.is_some() {
            match page.to_page.inverse() {
                Some(inv) => {
                    let mut q = [[0.0f32; 2]; 4];
                    for i in 0..4 {
                        let (px, py) = inv.map(erase_pt[i][0], erase_pt[i][1]);
                        q[i] = [px, py];
                    }
                    q
                }
                None => erase_px_ocr,
            }
        } else {
            erase_px_ocr
        };

        let (dx0, dy0, dx1, dy1) = real_box.unwrap_or(ocr_box);
        let draw_w = (dx1 - dx0).max(0.5);
        let draw_h = (dy1 - dy0).max(0.5);

        // Anchos y CIDs por carácter.
        let mut per_char: Vec<(char, usize, u16, f32)> = Vec::with_capacity(cand.text.len());
        let mut width_em = 0.0f32;
        for ch in cand.text.chars() {
            let Some((font_idx, _)) = fonts.resolve(ch) else {
                continue;
            };
            let Some(subset) = subsets.get(font_idx).and_then(|s| s.as_ref()) else {
                continue;
            };
            let Some(gid) = subset.gid_for_char.get(&ch).copied() else {
                continue;
            };
            let w = subset
                .widths
                .get(gid as usize)
                .copied()
                .unwrap_or(subset.units_per_em / 2) as f32
                / 1000.0;
            width_em += w;
            per_char.push((ch, font_idx, gid, w));
        }
        if per_char.is_empty() || width_em <= 0.0 {
            page_skips[cand.page].push(SkippedLine {
                line: cand.line,
                text: line.text.clone(),
                reason: SkipReason::MissingGlyphs(Vec::new()),
            });
            continue;
        }

        // Tamaño y posición: fuente y línea base del PDF de origen si existen;
        // si no, se estiman a partir de la caja del OCR.
        let descent = per_char
            .first()
            .and_then(|(_, f, _, _)| subsets.get(*f).and_then(|s| s.as_ref()))
            .map(|s| (-s.descent as f32 / s.units_per_em.max(1) as f32).clamp(0.06, 0.32))
            .unwrap_or(0.18);

        let (mut size_pt, start_x, baseline_y) = match source {
            Some(src) => {
                let size = src.size_pt.max(2.0);
                let (bx, by) = src.baseline.unwrap_or((dx0, dy0 + descent * size));
                (size, bx, by)
            }
            None => {
                let size_h = draw_h * 0.95;
                let size_w = draw_w * 0.98 / width_em;
                let size = size_h.min(size_w).clamp(2.0, 400.0);
                (size, dx0, dy0 + descent * size)
            }
        };
        // Que el texto quepa en el ancho de la caja: la fuente sustituta puede
        // ser algo más ancha que la original.
        if width_em > 0.0 {
            size_pt = size_pt.min(draw_w * 1.05 / width_em);
        }
        size_pt = size_pt.clamp(2.0, 400.0);

        // Runs por fuente.
        let mut runs: Vec<TextRun> = Vec::new();
        let mut cursor = start_x;
        for (_, font_idx, gid, w) in &per_char {
            let advance = *w * size_pt;
            match runs.last_mut() {
                Some(run) if run.font == *font_idx => {
                    run.cids.push(*gid);
                    run.advance += advance;
                }
                _ => runs.push(TextRun {
                    font: *font_idx,
                    x: cursor,
                    cids: vec![*gid],
                    advance,
                }),
            }
            cursor += advance;
        }

        let (bold, italic) = source.map(|s| (s.bold, s.italic)).unwrap_or((false, false));
        let source = source.cloned();

        page_lines[cand.page].push(LinePlan {
            line: cand.line,
            text: cand.text.to_string(),
            erase_px,
            erase_pt,
            background,
            uniform_background: uniform,
            text_color,
            size_pt,
            baseline: (start_x, baseline_y),
            runs,
            bold,
            italic,
            source,
        });
    }

    let mut pages_out = Vec::with_capacity(pages.len());
    for (idx, page) in pages.iter().enumerate() {
        let lines = std::mem::take(&mut page_lines[idx]);
        let non_uniform = lines.iter().any(|l| !l.uniform_background);
        pages_out.push(PagePlan {
            page: idx,
            overlay: page.overlay(),
            non_uniform,
            lines,
            skipped: std::mem::take(&mut page_skips[idx]),
        });
    }

    Ok(Plan {
        pages: pages_out,
        subsets,
        font_labels,
    })
}

/// Aplica el borrado real de píxeles a una página (solo si `mode` es `Real` y
/// la página se reconstruye desde el raster).
pub fn erase_pixels(page: &PagePlan, mode: EraseMode, img: &mut RgbImage) {
    if mode != EraseMode::Real || page.overlay {
        return;
    }
    for line in &page.lines {
        fill_quad(img, &line.erase_px, line.background);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::rect_quad;

    fn page_with_line(text: &str) -> PageContent {
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_pixel(200, 100, image::Rgb([250, 250, 250])));
        let mut page = PageContent::from_image(img);
        page.lines.push(OcrLine {
            points: [(20.0, 40.0), (180.0, 40.0), (180.0, 60.0), (20.0, 60.0)],
            text: text.into(),
            det_score: 1.0,
            rec_score: 1.0,
        });
        page
    }

    #[test]
    fn plan_borra_y_escala_el_texto() {
        let page = page_with_line("Hola mundo");
        let plan = build(&[page], &ReplaceOptions::default()).expect("plan");
        assert_eq!(plan.pages[0].lines.len(), 1);
        let line = &plan.pages[0].lines[0];
        assert!(line.size_pt > 2.0);
        assert!(line.runs.iter().any(|r| !r.cids.is_empty()));
        assert!(line.background.luminance() > 0.8);
        assert!(line.text_color.luminance() < 0.5);
    }

    #[test]
    fn cajas_excluidas_y_texto_vacio_se_omiten() {
        let mut page = page_with_line("uno");
        page.lines.push(OcrLine {
            points: {
                let q = rect_quad(20.0, 70.0, 180.0, 90.0);
                [
                    (q[0][0], q[0][1]),
                    (q[1][0], q[1][1]),
                    (q[2][0], q[2][1]),
                    (q[3][0], q[3][1]),
                ]
            },
            text: "  ".into(),
            det_score: 1.0,
            rec_score: 1.0,
        });
        let mut opts = ReplaceOptions::default();
        opts.excluded.push((0, 0));
        let plan = build(&[page], &opts).expect("plan");
        assert!(plan.pages[0].lines.is_empty());
        assert_eq!(plan.pages[0].skipped.len(), 2);
        assert_eq!(plan.pages[0].skipped[0].reason, SkipReason::Excluded);
        assert_eq!(plan.pages[0].skipped[1].reason, SkipReason::Empty);
    }

    #[test]
    fn sin_glifos_no_se_borra_la_linea() {
        // Un carácter de uso privado no existe en las fuentes incluidas.
        let page = page_with_line("hola \u{E000}");
        let plan = build(&[page], &ReplaceOptions::default()).expect("plan");
        assert!(plan.pages[0].lines.is_empty());
        match &plan.pages[0].skipped[0].reason {
            SkipReason::MissingGlyphs(chars) => assert!(chars.contains(&'\u{E000}')),
            other => panic!("motivo inesperado: {other:?}"),
        }
    }
}
