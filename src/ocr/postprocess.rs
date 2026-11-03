//! Post-procesado DB: mapa de probabilidad -> cajas de texto orientadas
//! (compatible con PaddleOCR). Adaptado de `gradan-hash/paddle-paddle-v6-ocr-rust`
//! (Apache-2.0), con la opción de aplicar sigmoide a la salida de la red.

use crate::ocr::config::DetParams;
use crate::ocr::geometry::{min_area_rect, order_box, point_in_quad, unclip, Pt};
use crate::ocr::preprocess::Preprocessed;
use image::{GrayImage, Luma};
use imageproc::contours::{find_contours, BorderType};

#[derive(Debug, Clone)]
pub struct DetectedBox {
    /// Cuadrilátero en píxeles de la imagen original, ordenado tl, tr, br, bl.
    pub points: [Pt; 4],
    pub score: f32,
}

/// `prob` es la salida de la red (H=net_h, W=net_w), en orden row-major.
/// Devuelve cajas mapeadas a la imagen original.
pub fn db_postprocess(
    prob: &[f32],
    net_w: usize,
    net_h: usize,
    pre: &Preprocessed,
    p: &DetParams,
) -> Vec<DetectedBox> {
    let prob: &[f32] = if p.apply_sigmoid {
        &prob
            .iter()
            .map(|v| 1.0 / (1.0 + (-v).exp()))
            .collect::<Vec<_>>()
    } else {
        prob
    };

    // 1. Binarización.
    let mut bitmap = GrayImage::new(net_w as u32, net_h as u32);
    for y in 0..net_h {
        for x in 0..net_w {
            let v = prob[y * net_w + x];
            bitmap.put_pixel(
                x as u32,
                y as u32,
                Luma([if v > p.thresh { 255 } else { 0 }]),
            );
        }
    }

    // 2. Contornos exteriores.
    let contours = find_contours::<u32>(&bitmap);

    let mut boxes = Vec::new();
    for contour in contours.into_iter() {
        if boxes.len() >= p.max_candidates {
            break;
        }
        if contour.border_type != BorderType::Outer || contour.points.len() < 4 {
            continue;
        }
        let pts: Vec<Pt> = contour
            .points
            .iter()
            .map(|pt| (pt.x as f32, pt.y as f32))
            .collect();

        // 3. Mini-box + filtro de tamaño.
        let (raw_box, sside) = min_area_rect(&pts);
        if sside < p.min_size {
            continue;
        }
        let raw_box = order_box(raw_box);

        // 4. Puntuación sobre el mapa de probabilidad.
        let score = box_score(prob, net_w, net_h, &raw_box);
        if score < p.box_thresh {
            continue;
        }

        // 5. Unclip y reajuste del rectángulo.
        let expanded = unclip(raw_box, p.unclip_ratio);
        let (final_box, sside2) = min_area_rect(&expanded);
        if sside2 < p.min_size + 2.0 {
            continue;
        }
        let final_box = order_box(final_box);

        // 6. Coordenadas de red -> imagen original y recorte.
        let ow = pre.orig_w as f32;
        let oh = pre.orig_h as f32;
        let mut mapped = [(0.0f32, 0.0f32); 4];
        for i in 0..4 {
            let x = (final_box[i].0 / pre.ratio_w).clamp(0.0, ow);
            let y = (final_box[i].1 / pre.ratio_h).clamp(0.0, oh);
            mapped[i] = (x.round(), y.round());
        }
        boxes.push(DetectedBox {
            points: mapped,
            score,
        });
    }
    boxes
}

/// Probabilidad media dentro del cuadrilátero (equivalente a `box_score_fast`):
/// rellena la caja dentro de su bbox alineado a ejes y promedia el mapa.
fn box_score(prob: &[f32], w: usize, h: usize, q: &[Pt; 4]) -> f32 {
    let xs = [q[0].0, q[1].0, q[2].0, q[3].0];
    let ys = [q[0].1, q[1].1, q[2].1, q[3].1];
    let xmin = xs.iter().cloned().fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let xmax = xs
        .iter()
        .cloned()
        .fold(f32::MIN, f32::max)
        .ceil()
        .min((w - 1) as f32) as usize;
    let ymin = ys.iter().cloned().fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let ymax = ys
        .iter()
        .cloned()
        .fold(f32::MIN, f32::max)
        .ceil()
        .min((h - 1) as f32) as usize;
    if xmax < xmin || ymax < ymin {
        return 0.0;
    }

    let mut sum = 0.0f32;
    let mut count = 0u32;
    for y in ymin..=ymax {
        for x in xmin..=xmax {
            if point_in_quad(x as f32 + 0.5, y as f32 + 0.5, q) {
                sum += prob[y * w + x];
                count += 1;
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}
