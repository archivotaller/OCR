//! Carga de imágenes desde disco (PNG, JPEG, BMP, GIF, WebP, TIFF multipágina, ...).

use anyhow::{bail, Context, Result};
use image::DynamicImage;
use std::fs::File;
use std::path::Path;

pub const SUPPORTED_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff", "pnm",
];

/// `true` si la extensión corresponde a una imagen soportada.
pub fn has_image_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Convierte un píxel CMYK (0 = sin tinta, 255 = tinta plena, convención de
/// TIFF) a RGB con la fórmula estándar `(255-C)·(255-K)/255`.
fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
    let k = 255 - k as u32;
    let ch = |v: u8| (((255 - v as u32) * k + 127) / 255) as u8;
    [ch(c), ch(m), ch(y)]
}

/// Convierte una página TIFF (raw) a `DynamicImage` RGB8, bajando muestras de
/// 16 bits y manejando gris de 1/2/4/8/16 bits, RGB/RGBA y CMYK/CMYKA.
fn page_to_image<R: std::io::Read + std::io::Seek>(
    dec: &mut tiff::decoder::Decoder<R>,
    w: u32,
    h: u32,
    buf: &[u8],
) -> Result<DynamicImage> {
    let ct = dec.colortype().with_context(|| "color de página TIFF")?;
    let (channels, depth) = match ct {
        tiff::ColorType::Gray(d) => (1u32, d),
        tiff::ColorType::RGB(d) => (3u32, d),
        tiff::ColorType::RGBA(d) => (4u32, d),
        tiff::ColorType::CMYK(d) | tiff::ColorType::CMYKA(d) => (4u32, d),
        tiff::ColorType::Palette(_) => {
            bail!("páginas TIFF con paleta no soportadas")
        }
        _ => bail!("tipo de color TIFF no soportado: {ct:?}"),
    };
    if depth != 1 && depth != 2 && depth != 4 && depth != 8 && depth != 16 {
        bail!("profundidad TIFF no soportada: {depth} bits");
    }

    // El decodificador entrega las filas empaquetadas: ceil(w·canales·bits/8).
    let row_bytes = (w as usize * channels as usize * depth as usize).div_ceil(8);
    let needed = row_bytes
        .checked_mul(h as usize)
        .context("dimensiones TIFF desbordadas")?;
    if buf.len() < needed {
        bail!(
            "buffer TIFF corto: {} bytes, se esperaban {}",
            buf.len(),
            needed
        );
    }

    let mut data = Vec::with_capacity((w as usize * h as usize * 3).max(1));

    // Gris de 1/2/4 bits: muestras empaquetadas MSB-first.
    if channels == 1 && depth < 8 {
        let max = ((1u32 << depth) - 1).max(1);
        for y in 0..h as usize {
            let row = &buf[y * row_bytes..(y + 1) * row_bytes];
            for x in 0..w as usize {
                let bit = x * depth as usize;
                let byte = row[bit / 8];
                let shift = 8 - depth as usize - (bit % 8);
                let v = ((byte >> shift) as u32) & max;
                let g = (v * 255 / max) as u8;
                data.extend_from_slice(&[g, g, g]);
            }
        }
        let img = image::RgbImage::from_raw(w, h, data).with_context(|| "forma RGB válida")?;
        return Ok(DynamicImage::ImageRgb8(img));
    }

    let sample_bytes = (depth / 8) as usize;
    let pixel_bytes = channels as usize * sample_bytes;
    for y in 0..h as usize {
        let row = &buf[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..w as usize {
            let off = x * pixel_bytes;
            let sample = |i: usize| -> u8 {
                if depth == 16 {
                    let b = i * 2;
                    (u16::from_ne_bytes([row[off + b], row[off + b + 1]]) >> 8) as u8
                } else {
                    row[off + i]
                }
            };
            let rgb = if channels == 1 {
                let g = sample(0);
                [g, g, g]
            } else if matches!(ct, tiff::ColorType::CMYK(_) | tiff::ColorType::CMYKA(_)) {
                cmyk_to_rgb(sample(0), sample(1), sample(2), sample(3))
            } else {
                [sample(0), sample(1), sample(2)]
            };
            data.extend_from_slice(&rgb);
        }
    }
    let img = image::RgbImage::from_raw(w, h, data).with_context(|| "forma RGB válida")?;
    Ok(DynamicImage::ImageRgb8(img))
}

/// Carga una o más imágenes (los TIFF multipágina devuelven un frame por página).
pub fn load_images(path: &Path) -> Result<Vec<DynamicImage>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();

    if ext == "tif" || ext == "tiff" {
        let mut dec = tiff::decoder::Decoder::new(
            File::open(path).with_context(|| format!("abriendo {}", path.display()))?,
        )
        .with_context(|| format!("decodificando {}", path.display()))?;

        let mut pages = Vec::new();
        loop {
            let (w, h) = dec
                .dimensions()
                .with_context(|| "dimensiones de página TIFF")?;
            let mut result = dec
                .read_image()
                .with_context(|| format!("leyendo página de {}", path.display()))?;
            let bytes = result.as_buffer(0).as_bytes().to_vec();
            pages.push(page_to_image(&mut dec, w, h, &bytes)?);
            if !dec.more_images() {
                break;
            }
            dec.next_image()
                .with_context(|| format!("avanzando página de {}", path.display()))?;
        }
        if pages.is_empty() {
            bail!("TIFF vacío: {}", path.display());
        }
        return Ok(pages);
    }

    let img = image::open(path).with_context(|| format!("abriendo {}", path.display()))?;
    Ok(vec![img])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_primarios() {
        // Convención TIFF: 0 = sin tinta, 255 = tinta plena.
        assert_eq!(cmyk_to_rgb(0, 0, 0, 0), [255, 255, 255]);
        assert_eq!(cmyk_to_rgb(0, 255, 255, 0), [255, 0, 0]);
        assert_eq!(cmyk_to_rgb(255, 0, 255, 0), [0, 255, 0]);
        assert_eq!(cmyk_to_rgb(255, 255, 0, 0), [0, 0, 255]);
        assert_eq!(cmyk_to_rgb(0, 0, 0, 255), [0, 0, 0]);
        // Tinta al 50 %: gris medio.
        assert_eq!(cmyk_to_rgb(0, 0, 0, 128), [127, 127, 127]);
    }
}
