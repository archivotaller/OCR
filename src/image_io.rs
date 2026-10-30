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

/// Convierte una página TIFF (raw) a `DynamicImage` RGB8, bajando muestras de
/// 16 bits y manejando gris/RGB/RGBA/CMYK.
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
    let per_px = channels as usize * depth as usize / 8;

    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for row in 0..h {
        let row_off = (row * w * per_px as u32) as usize;
        for x in 0..w {
            let off = row_off + x as usize * per_px;
            let sample = |i: usize| -> Result<u8> {
                match depth {
                    8 => Ok(buf[off + i]),
                    16 => {
                        let v = u16::from_ne_bytes([buf[off + i * 2], buf[off + i * 2 + 1]]);
                        Ok((v >> 8) as u8)
                    }
                    _ => bail!("profundidad TIFF no soportada: {depth} bits"),
                }
            };
            let (mut r, mut g, mut b) = if channels == 1 {
                let g = sample(0)?;
                (g, g, g)
            } else {
                (sample(0)?, sample(1)?, sample(2)?)
            };
            if matches!(ct, tiff::ColorType::CMYK(_) | tiff::ColorType::CMYKA(_)) {
                let k = sample(3)? as u32;
                r = ((r as u32).saturating_mul(k) / 255) as u8;
                g = ((g as u32).saturating_mul(k) / 255) as u8;
                b = ((b as u32).saturating_mul(k) / 255) as u8;
            }
            data.extend_from_slice(&[r, g, b]);
        }
    }
    let img = image::RgbImage::from_raw(w, h, data)
        .with_context(|| "forma RGB válida")?;
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
        let mut dec = tiff::decoder::Decoder::new(File::open(path).with_context(|| {
            format!("abriendo {}", path.display())
        })?)
        .with_context(|| format!("decodificando {}", path.display()))?;

        let mut pages = Vec::new();
        loop {
            let (w, h) = dec.dimensions().with_context(|| "dimensiones de página TIFF")?;
            let mut result = dec
                .read_image()
                .with_context(|| format!("leyendo página de {}", path.display()))?;
            let bytes = result
                .as_buffer(0)
                .as_bytes()
                .to_vec();
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
