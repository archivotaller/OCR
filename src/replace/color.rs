//! Muestreo de color para el reemplazo de texto.
//!
//! El color de fondo de cada caja se obtiene del **anillo** que rodea la línea
//! (no de dentro, donde está el texto): se agrupan los píxeles en cubos y se
//! toma el grupo dominante. Así funciona tanto con fondos planos como con
//! degradados suaves, y se detecta cuándo el fondo no es uniforme.
//!
//! El color del texto se estima con los píxeles "de tinta" del interior de la
//! caja (los que más se separan del fondo) y se corrige si el contraste es bajo.

use crate::geom::{expand_quad, point_in_quad, quad_bounds, Quad};
use image::RgbImage;
use std::collections::HashMap;
use std::fmt;

/// Color RGB de 8 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };
    pub const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };

    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Rgb { r, g, b }
    }

    /// Interpreta `#rrggbb`, `rrggbb` o los nombres `auto`, `black`, `white`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let t = s.trim().to_ascii_lowercase();
        match t.as_str() {
            "auto" => None,
            "black" | "negro" => Some(Rgb::BLACK),
            "white" | "blanco" => Some(Rgb::WHITE),
            _ => {
                let hex = t.strip_prefix('#').unwrap_or(&t);
                if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return None;
                }
                let v = u32::from_str_radix(hex, 16).ok()?;
                Some(Rgb {
                    r: (v >> 16) as u8,
                    g: (v >> 8) as u8,
                    b: v as u8,
                })
            }
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// Distancia euclídea en RGB (0 = iguales, ~441 = máximo).
    pub fn distance(self, other: Rgb) -> f32 {
        let dr = self.r as f32 - other.r as f32;
        let dg = self.g as f32 - other.g as f32;
        let db = self.b as f32 - other.b as f32;
        (dr * dr + dg * dg + db * db).sqrt()
    }

    /// Luminancia percibida (0 = negro, 1 = blanco).
    pub fn luminance(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }

    /// Color de texto con contraste suficiente sobre `self`.
    pub fn contrast_text(self) -> Rgb {
        if self.luminance() > 0.5 {
            Rgb::new(24, 24, 24)
        } else {
            Rgb::new(245, 245, 245)
        }
    }

    /// Contraste WCAG aproximado entre dos colores (1 = idénticos, 21 = máximo).
    pub fn contrast_ratio(self, other: Rgb) -> f32 {
        let l = |c: Rgb| {
            let f = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
        };
        let (a, b) = (l(self), l(other));
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

/// Resultado del muestreo de fondo de una caja.
#[derive(Debug, Clone, Copy)]
pub struct BackgroundSample {
    pub color: Rgb,
    /// Fracción de píxeles del anillo que comparten el color dominante.
    pub uniformity: f32,
}

impl BackgroundSample {
    /// `true` si el fondo es prácticamente liso.
    pub fn uniform(&self) -> bool {
        self.uniformity >= 0.75
    }
}

#[inline]
fn pixel(img: &RgbImage, x: i64, y: i64) -> Option<Rgb> {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return None;
    }
    let p = img.get_pixel(x as u32, y as u32).0;
    Some(Rgb::new(p[0], p[1], p[2]))
}

fn bucket(c: Rgb) -> u8 {
    // 8 niveles por canal: suficiente para separar fondos y texto, y estable
    // frente al ruido de compresión.
    ((c.r >> 5) << 6) | ((c.g >> 5) << 3) | (c.b >> 5)
}

/// Color dominante de los píxeles de `polygon` (o de su anillo).
///
/// Devuelve el color medio del grupo dominante y la fracción de píxeles que
/// caen en él. Con `inner` presente solo se consideran los píxeles dentro de
/// `outer` y fuera de `inner` (anillo).
fn dominant_color(img: &RgbImage, outer: &Quad, inner: Option<&Quad>) -> Option<BackgroundSample> {
    let (x0, y0, x1, y1) = quad_bounds(outer);
    let x0 = x0.floor().max(0.0) as i64;
    let y0 = y0.floor().max(0.0) as i64;
    let x1 = x1.ceil().min(img.width() as f32) as i64;
    let y1 = y1.ceil().min(img.height() as f32) as i64;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let mut groups: HashMap<u8, (u64, u64, u64, u64)> = HashMap::new();
    let mut total = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let (xc, yc) = (x as f32 + 0.5, y as f32 + 0.5);
            if !point_in_quad(xc, yc, outer) {
                continue;
            }
            if let Some(inner) = inner {
                if point_in_quad(xc, yc, inner) {
                    continue;
                }
            }
            let Some(c) = pixel(img, x, y) else {
                continue;
            };
            let g = groups.entry(bucket(c)).or_insert((0, 0, 0, 0));
            g.0 += c.r as u64;
            g.1 += c.g as u64;
            g.2 += c.b as u64;
            g.3 += 1;
            total += 1;
        }
    }
    if total == 0 {
        return None;
    }
    let (_, (sr, sg, sb, n)) = groups.into_iter().max_by_key(|(_, v)| v.3)?;
    let n = n.max(1);
    Some(BackgroundSample {
        color: Rgb::new((sr / n) as u8, (sg / n) as u8, (sb / n) as u8),
        uniformity: n as f32 / total as f32,
    })
}

/// Muestrea el fondo alrededor de `quad` (en píxeles de la imagen).
///
/// `margin` es la separación entre la caja y el anillo de muestreo, y
/// `band` el grosor del anillo.
pub fn sample_background(img: &RgbImage, quad: &Quad, margin: f32, band: f32) -> BackgroundSample {
    let inner = expand_quad(quad, margin.max(0.0));
    let outer = expand_quad(quad, margin.max(0.0) + band.max(2.0));
    dominant_color(img, &outer, Some(&inner))
        .or_else(|| dominant_color(img, &outer, None))
        .unwrap_or(BackgroundSample {
            color: Rgb::WHITE,
            uniformity: 0.0,
        })
}

/// Estima el color del texto dentro de `quad` a partir de los píxeles que más
/// se separan de `background`.
pub fn sample_text_color(img: &RgbImage, quad: &Quad, background: Rgb) -> Rgb {
    let (x0, y0, x1, y1) = quad_bounds(quad);
    let x0 = x0.floor().max(0.0) as i64;
    let y0 = y0.floor().max(0.0) as i64;
    let x1 = x1.ceil().min(img.width() as f32) as i64;
    let y1 = y1.ceil().min(img.height() as f32) as i64;

    let mut sum = (0u64, 0u64, 0u64, 0u64);
    let mut best: Option<(f32, Rgb)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            let (xc, yc) = (x as f32 + 0.5, y as f32 + 0.5);
            if !point_in_quad(xc, yc, quad) {
                continue;
            }
            let Some(c) = pixel(img, x, y) else {
                continue;
            };
            let d = c.distance(background);
            if d < 48.0 {
                continue;
            }
            sum.0 += c.r as u64;
            sum.1 += c.g as u64;
            sum.2 += c.b as u64;
            sum.3 += 1;
            if best.map(|(bd, _)| d > bd).unwrap_or(true) {
                best = Some((d, c));
            }
        }
    }

    if sum.3 >= 4 {
        let ink = Rgb::new(
            (sum.0 / sum.3) as u8,
            (sum.1 / sum.3) as u8,
            (sum.2 / sum.3) as u8,
        );
        if ink.contrast_ratio(background) >= 2.5 {
            return ink;
        }
    }
    if let Some((_, c)) = best {
        if c.contrast_ratio(background) >= 2.5 {
            return c;
        }
    }
    background.contrast_text()
}

/// Rellena en la imagen el polígono `quad` con `color` (borrado real).
///
/// Solo se tocan los píxeles del interior del cuadrilátero; el resto de la
/// imagen queda intacto.
pub fn fill_quad(img: &mut RgbImage, quad: &Quad, color: Rgb) {
    let (x0, y0, x1, y1) = quad_bounds(quad);
    let x0 = x0.floor().max(0.0) as i64;
    let y0 = y0.floor().max(0.0) as i64;
    let x1 = x1.ceil().min(img.width() as f32) as i64;
    let y1 = y1.ceil().min(img.height() as f32) as i64;
    let p = image::Rgb([color.r, color.g, color.b]);
    for y in y0..y1 {
        for x in x0..x1 {
            if point_in_quad(x as f32 + 0.5, y as f32 + 0.5, quad) {
                img.put_pixel(x as u32, y as u32, p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::rect_quad;

    #[test]
    fn parse_y_formato_de_colores() {
        assert_eq!(Rgb::parse("#102030"), Some(Rgb::new(16, 32, 48)));
        assert_eq!(Rgb::parse("white"), Some(Rgb::WHITE));
        assert_eq!(Rgb::parse("auto"), None);
        assert_eq!(Rgb::new(16, 32, 48).to_hex(), "#102030");
        assert_eq!(Rgb::parse("no-es-un-color"), None);
    }

    #[test]
    fn contraste_elige_texto_legible() {
        assert_eq!(Rgb::WHITE.contrast_text(), Rgb::new(24, 24, 24));
        assert_eq!(Rgb::BLACK.contrast_text(), Rgb::new(245, 245, 245));
        assert!(Rgb::WHITE.contrast_ratio(Rgb::BLACK) > 20.0);
    }

    #[test]
    fn fondo_dominante_en_anillo() {
        // Fondo azul con una banda de texto negro en el centro.
        let mut img = RgbImage::from_pixel(60, 40, image::Rgb([10, 20, 200]));
        for y in 15..25 {
            for x in 10..50 {
                img.put_pixel(x, y, image::Rgb([0, 0, 0]));
            }
        }
        let q = rect_quad(10.0, 15.0, 50.0, 25.0);
        let sample = sample_background(&img, &q, 2.0, 4.0);
        assert!(
            sample.color.b > 150 && sample.color.r < 40,
            "fondo inesperado: {}",
            sample.color
        );
        assert!(sample.uniform(), "el anillo debería ser uniforme");
        let text = sample_text_color(&img, &q, sample.color);
        assert!(
            text.contrast_ratio(sample.color) >= 2.5,
            "texto con poco contraste: {text} sobre {}",
            sample.color
        );
    }

    #[test]
    fn borrado_solo_toca_el_interior() {
        let mut img = RgbImage::from_pixel(20, 20, image::Rgb([200, 30, 40]));
        let q = rect_quad(5.0, 5.0, 10.0, 10.0);
        fill_quad(&mut img, &q, Rgb::WHITE);
        assert_eq!(img.get_pixel(7, 7).0, [255, 255, 255]);
        assert_eq!(img.get_pixel(0, 0).0, [200, 30, 40]);
        assert_eq!(img.get_pixel(15, 15).0, [200, 30, 40]);
    }
}
