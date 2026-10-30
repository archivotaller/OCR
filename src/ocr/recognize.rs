//! Reconocimiento: rectificación por perspectiva de cada cuadrilátero,
//! redimensionado/normalizado para la red de reconocimiento y decodificación
//! CTC (greedy) a texto.
//!
//! Adaptado de `gradan-hash/paddle-paddle-v6-ocr-rust` (Apache-2.0).

use crate::ocr::geometry::{dist, Pt};
use image::{DynamicImage, ImageBuffer, Rgb, RgbImage};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use tract_onnx::prelude::*;

/// Altura de entrada de la red de reconocimiento (PP-OCRv6: 48). El ancho es
/// dinámico pero se ajusta a una escalera fija para reutilizar planes.
pub const REC_HEIGHT: u32 = 48;
const WIDTH_BUCKETS: [u32; 8] = [160, 256, 384, 512, 768, 1024, 1536, 2048];
const MAX_WIDTH: u32 = 2048;

/// La escalera fija de anchos (para pre-warm de planes).
pub fn width_buckets() -> &'static [u32] {
    &WIDTH_BUCKETS
}

/// Menor bucket >= w (acotado a MAX_WIDTH).
fn bucket_width(w: u32) -> u32 {
    for &b in &WIDTH_BUCKETS {
        if w <= b {
            return b;
        }
    }
    MAX_WIDTH
}

/// Recorta y rectifica por perspectiva un cuadrilátero de la imagen origen a
/// una tira RGB vertical (equivalente a `get_rotate_crop_image`).
pub fn rectify_crop(src: &RgbImage, quad: &[Pt; 4], pad: bool) -> RgbImage {
    let quad = if pad {
        padded_quad(src, quad)
    } else {
        *quad
    };
    let w = dist(quad[0], quad[1])
        .max(dist(quad[3], quad[2]))
        .round()
        .max(1.0);
    let h = dist(quad[0], quad[3])
        .max(dist(quad[1], quad[2]))
        .round()
        .max(1.0);
    let (cw, ch) = (w as u32, h as u32);

    let dst = [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)];
    // imageproc `warp` espera una proyección INPUT -> OUTPUT (cuad -> rect).
    let proj = match Projection::from_control_points(quad, dst) {
        Some(p) => p,
        None => return ImageBuffer::from_pixel(cw.max(1), ch.max(1), Rgb([0, 0, 0])),
    };

    let mut out: RgbImage = ImageBuffer::new(cw, ch);
    warp_into(
        src,
        &proj,
        Interpolation::Bilinear,
        Rgb([0, 0, 0]),
        &mut out,
    );

    // Texto vertical: rotar para que se lea de izquierda a derecha.
    if (ch as f32) >= (cw as f32) * 1.5 {
        image::imageops::rotate90(&out)
    } else {
        out
    }
}

fn padded_quad(src: &RgbImage, quad: &[Pt; 4]) -> [Pt; 4] {
    let w = dist(quad[0], quad[1]).max(dist(quad[3], quad[2])).max(1.0);
    let h = dist(quad[0], quad[3]).max(dist(quad[1], quad[2])).max(1.0);
    let ux = ((quad[1].0 - quad[0].0) / w, (quad[1].1 - quad[0].1) / w);
    let vx = ((quad[3].0 - quad[0].0) / h, (quad[3].1 - quad[0].1) / h);
    let xpad = h * 0.25;
    let ypad = h * 0.55;
    let max_x = src.width().saturating_sub(1) as f32;
    let max_y = src.height().saturating_sub(1) as f32;
    let clamp = |(x, y): Pt| (x.clamp(0.0, max_x), y.clamp(0.0, max_y));

    [
        clamp((
            quad[0].0 - ux.0 * xpad - vx.0 * ypad,
            quad[0].1 - ux.1 * xpad - vx.1 * ypad,
        )),
        clamp((
            quad[1].0 + ux.0 * xpad - vx.0 * ypad,
            quad[1].1 + ux.1 * xpad - vx.1 * ypad,
        )),
        clamp((
            quad[2].0 + ux.0 * xpad + vx.0 * ypad,
            quad[2].1 + ux.1 * xpad + vx.1 * ypad,
        )),
        clamp((
            quad[3].0 - ux.0 * xpad + vx.0 * ypad,
            quad[3].1 - ux.1 * xpad + vx.1 * ypad,
        )),
    ]
}

/// Redimensiona una tira rectificada al tensor de rec (1,3,48,W), BGR,
/// normalizado a [-1,1]. Devuelve (tensor, ancho_pad).
pub fn rec_preprocess(crop: &RgbImage) -> (Tensor, u32) {
    let (w, h) = (crop.width().max(1), crop.height().max(1));
    let resized_w = ((REC_HEIGHT as f32) * (w as f32) / (h as f32)).ceil() as u32;
    let resized_w = resized_w.max(1);

    // Ajusta el ancho con pad a un bucket fijo para reutilizar planes.
    let pad_w = bucket_width(resized_w);
    let content_w = resized_w.min(pad_w);

    let scaled = DynamicImage::ImageRgb8(crop.clone())
        .resize_exact(content_w, REC_HEIGHT, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let (rh, rw) = (REC_HEIGHT as usize, pad_w as usize);
    let plane = rh * rw;
    let mut data = vec![0f32; 3 * plane]; // 0 = padding
    for (x, y, px) in scaled.enumerate_pixels() {
        let idx = y as usize * rw + x as usize;
        let [r, g, b] = px.0;
        let bgr = [b, g, r];
        for c in 0..3 {
            data[c * plane + idx] = (bgr[c] as f32 / 255.0 - 0.5) / 0.5;
        }
    }
    let tensor = Tensor::from_shape(&[1, 3, rh, rw], &data).expect("forma de rec válida");
    (tensor, pad_w)
}

/// Divide una tira larga en tiras de palabras usando el espacio vertical en
/// blanco. PP-OCR detecta como línea, pero el reconocedor suele omitir los
/// espacios; re-reconocer recortes de palabra ancho preserva espacios y mejora
/// letras ambiguas (más resolución horizontal por palabra).
pub fn split_word_crops(crop: &RgbImage) -> Vec<RgbImage> {
    let (w, h) = (crop.width(), crop.height());
    if w < 160 || h < 16 || (w as f32 / h as f32) < 5.0 {
        return Vec::new();
    }

    let mut ink = vec![0f32; w as usize];
    for x in 0..w {
        let mut count = 0u32;
        for y in 0..h {
            let [r, g, b] = crop.get_pixel(x, y).0;
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if luma < 120.0 {
                count += 1;
            }
        }
        ink[x as usize] = count as f32;
    }

    let radius = 3usize;
    let mut smooth = vec![0f32; w as usize];
    for i in 0..w as usize {
        let start = i.saturating_sub(radius);
        let end = (i + radius + 1).min(w as usize);
        smooth[i] = ink[start..end].iter().sum::<f32>() / (end - start) as f32;
    }

    let active_min = (h as f32 * 0.025).max(1.5);
    let active: Vec<bool> = smooth.iter().map(|&v| v > active_min).collect();
    let gap_min = ((h as f32 * 0.25).round() as u32).max(20) as usize;
    let edge_pad = ((h as f32 * 0.10).round() as u32).clamp(6, 14);

    let mut splits = Vec::new();
    let mut i = 0usize;
    while i < active.len() {
        let val = active[i];
        let start = i;
        while i < active.len() && active[i] == val {
            i += 1;
        }
        if !val && i - start >= gap_min && start > gap_min && i < active.len() - gap_min {
            splits.push((start + i) / 2);
        }
    }

    if splits.is_empty() || splits.len() > 7 {
        return Vec::new();
    }

    let mut bounds = Vec::with_capacity(splits.len() + 2);
    bounds.push(0usize);
    bounds.extend(splits);
    bounds.push(w as usize);

    let mut out = Vec::new();
    for pair in bounds.windows(2) {
        let (mut left, mut right) = (pair[0], pair[1]);
        while left < right && smooth[left] <= active_min {
            left += 1;
        }
        while right > left && smooth[right - 1] <= active_min {
            right -= 1;
        }
        left = left.saturating_sub(edge_pad as usize);
        right = (right + edge_pad as usize).min(w as usize);
        if right > left && right - left >= (h as usize / 2).max(16) {
            out.push(
                image::imageops::crop_imm(crop, left as u32, 0, (right - left) as u32, h)
                    .to_image(),
            );
        }
    }

    if out.len() >= 2 {
        out
    } else {
        Vec::new()
    }
}

/// Tabla de caracteres para el decodificador CTC: índice 0 = blank, luego el
/// diccionario y, al final, un espacio.
pub struct Charset {
    chars: Vec<String>,
}

impl Charset {
    pub fn from_dict(dict: &str) -> Self {
        let mut chars = Vec::with_capacity(dict.lines().count() + 2);
        chars.push(String::new()); // índice 0 = blank
        for line in dict.split('\n') {
            // Los diccionarios pueden usar CRLF; descartamos el \r.
            let line = line.trim_end_matches('\r');
            if !line.is_empty() {
                chars.push(line.to_string());
            } else {
                chars.push(String::new());
            }
        }
        // Elimina una posible entrada vacía final por el último newline.
        if chars.last().map(|s| s.is_empty()).unwrap_or(false) {
            chars.pop();
        }
        chars.push(" ".to_string()); // sentinela de espacio de PaddleOCR
        Charset { chars }
    }

    pub fn num_classes(&self) -> usize {
        self.chars.len()
    }

    /// Decodificación CTC greedy de una matriz (T, C). Devuelve (texto, conf).
    pub fn ctc_decode(&self, probs: &[f32], t: usize, c: usize) -> (String, f32) {
        let mut text = String::new();
        let mut sum_conf = 0.0f32;
        let mut n_conf = 0u32;
        let mut last_idx = 0usize; // blank
        for step in 0..t {
            let row = &probs[step * c..step * c + c];
            let (mut best, mut best_p) = (0usize, f32::MIN);
            for (i, &p) in row.iter().enumerate() {
                if p > best_p {
                    best_p = p;
                    best = i;
                }
            }
            if best != 0 && best != last_idx {
                if let Some(ch) = self.chars.get(best) {
                    text.push_str(ch);
                }
                sum_conf += best_p;
                n_conf += 1;
            }
            last_idx = best;
        }
        let conf = if n_conf > 0 {
            sum_conf / n_conf as f32
        } else {
            0.0
        };
        (text, conf)
    }
}

/// Verifica que el número de clases del modelo coincida con el charset.
pub fn ctc_classes_ok(charset: &Charset, model_classes: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        charset.num_classes() == model_classes,
        "el charset tiene {} clases pero el modelo emite {}",
        charset.num_classes(),
        model_classes
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctc_basic() {
        // dict: ["a","b"] -> charset = ["", "a", "b", " "]
        let c = Charset::from_dict("a\nb\n");
        assert_eq!(c.num_classes(), 4);
        // 3 pasos de tiempo: t0="a", t1="a" (colapso), t2="b"
        // prob por clase: [blank, a, b, sp]
        let mut probs = vec![0.0f32; 3 * 4];
        probs[0 * 4 + 1] = 0.9; // a
        probs[1 * 4 + 1] = 0.9; // a (repite -> colapsa)
        probs[2 * 4 + 2] = 0.8; // b
        let (text, conf) = c.ctc_decode(&probs, 3, 4);
        assert_eq!(text, "ab");
        // 0.9 (a) y 0.8 (b): la repetición de "a" colapsa sin sumar.
        assert!((conf - 0.85).abs() < 0.01, "conf={conf}");
    }

    #[test]
    fn ctc_blank_collapse() {
        let c = Charset::from_dict("a\nb\n");
        // a, blank, b -> "ab"
        let mut probs = vec![0.0f32; 3 * 4];
        probs[0] = 0.0;
        probs[0 * 4 + 1] = 0.9;
        probs[1 * 4 + 0] = 0.9; // blank
        probs[2 * 4 + 2] = 0.8;
        let (text, _) = c.ctc_decode(&probs, 3, 4);
        assert_eq!(text, "ab");
    }

    #[test]
    fn ctc_rep_char() {
        // "aa" necesita blank en medio: a, blank, a
        let c = Charset::from_dict("a\nb\n");
        let mut probs = vec![0.0f32; 3 * 4];
        probs[0 * 4 + 1] = 0.9;
        probs[1 * 4 + 0] = 0.9;
        probs[2 * 4 + 1] = 0.9;
        let (text, _) = c.ctc_decode(&probs, 3, 4);
        assert_eq!(text, "aa");
    }

    #[test]
    fn rec_preprocess_shape() {
        let img = ImageBuffer::from_pixel(320, 48, Rgb([200, 100, 50]));
        let (tensor, pad_w) = rec_preprocess(&img);
        // 320 -> bucket de 384 (escalera de anchos fijos).
        assert_eq!(tensor.shape(), &[1, 3, 48, 384]);
        assert_eq!(pad_w, 384);
    }

    #[test]
    fn bucket_width_ok() {
        assert_eq!(bucket_width(100), 160);
        assert_eq!(bucket_width(160), 160);
        assert_eq!(bucket_width(300), 384);
        assert_eq!(bucket_width(9000), 2048);
    }
}
