//! Pre-procesado de la imagen para el detector: DetResizeForTest +
//! NormalizeImage + ToCHWImage (equivalente al `inference.yml` de PP-OCR).
//!
//! Adaptado de `gradan-hash/paddle-paddle-v6-ocr-rust` (Apache-2.0).

use crate::ocr::config::{DetConfig, LimitType};
use image::DynamicImage;
use tract_onnx::prelude::*;

pub struct Preprocessed {
    pub tensor: Tensor,
    /// Tamaño de entrada a la red.
    pub resize_w: u32,
    pub resize_h: u32,
    /// Factores red -> original: orig = net / ratio.
    pub ratio_w: f32,
    pub ratio_h: f32,
    pub orig_w: u32,
    pub orig_h: u32,
}

/// Dimensiones redimensionadas (múltiplos de 32) según DetResizeForTest.
fn resize_dims(w: u32, h: u32, p: &crate::ocr::config::DetParams) -> (u32, u32) {
    let (wf, hf) = (w as f32, h as f32);
    let limit = p.limit_side_len as f32;
    let ratio = match p.limit_type {
        LimitType::Max => {
            let m = wf.max(hf);
            if m > limit {
                limit / m
            } else {
                1.0
            }
        }
        LimitType::Min => {
            let m = wf.min(hf);
            if m < limit {
                limit / m
            } else {
                1.0
            }
        }
    };
    // Floor de 64: los ONNX del CDN no compilan con tract si algún lado < 64
    // (fallan los passes ChangeAxes / la unificación del Concat de la FPN).
    let round32 = |v: f32| (((v / 32.0).round() as i32).max(2) * 32) as u32;
    (round32(wf * ratio), round32(hf * ratio))
}

pub fn preprocess(img: &DynamicImage, cfg: &DetConfig) -> Preprocessed {
    let p = &cfg.params;
    let n = &cfg.normalize;
    let orig_w = img.width();
    let orig_h = img.height();
    let (resize_w, resize_h) = resize_dims(orig_w, orig_h, p);

    let resized = img
        .resize_exact(resize_w, resize_h, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let (rw, rh) = (resize_w as usize, resize_h as usize);
    // Buffer CHW en f32, orden de canales según `n.bgr`.
    let mut data = vec![0f32; 3 * rh * rw];
    let plane = rh * rw;
    for (x, y, px) in resized.enumerate_pixels() {
        let idx = y as usize * rw + x as usize;
        let [r, g, b] = px.0;
        let chans = if n.bgr { [b, g, r] } else { [r, g, b] };
        for c in 0..3 {
            let v = chans[c] as f32 * n.scale;
            data[c * plane + idx] = (v - n.mean[c]) / n.std[c];
        }
    }

    let tensor = Tensor::from_shape(&[1, 3, rh, rw], &data).expect("forma CHW válida");

    Preprocessed {
        tensor,
        resize_w,
        resize_h,
        ratio_w: resize_w as f32 / orig_w as f32,
        ratio_h: resize_h as f32 / orig_h as f32,
        orig_w,
        orig_h,
    }
}
