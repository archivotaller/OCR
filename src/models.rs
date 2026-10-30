//! Manifesto y caché de modelos ONNX (PP-OCRv6) descargados desde el CDN del
//! proyecto `paddle-paddle-v6-ocr-rust` (Apache-2.0).
//!
//! En la primera ejecución se descargan los pesos, se verifican contra su
//! SHA-256 y se cachean bajo `%LOCALAPPDATA%\octxt\models` (Windows) o
//! `~/.cache/octxt/models`. Ejecuciones posteriores funcionan sin red.
//!
//! Se usan los ONNX re-exportados del CDN (`cdn.drmhse.com`) en lugar de los
//! `inference.onnx` oficiales de HuggingFace: los oficiales exportan las
//! formas con símbolos/expresiones (`floor`, `ceil`) que el shape-inference de
//! tract no puede resolver, mientras que estos cargan y corren correctamente
//! fijando la fact de entrada concreta (ver `ocr::engine::Compiled`).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;

/// Tier del modelo PP-OCRv6.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Tiny,
    Small,
    Medium,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Tiny => "tiny",
            Tier::Small => "small",
            Tier::Medium => "medium",
        }
    }

    pub fn det_id(self) -> &'static str {
        match self {
            Tier::Tiny => "PP-OCRv6_tiny_det",
            Tier::Small => "PP-OCRv6_small_det",
            Tier::Medium => "PP-OCRv6_medium_det",
        }
    }

    pub fn rec_id(self) -> &'static str {
        match self {
            Tier::Tiny => "PP-OCRv6_tiny_rec",
            Tier::Small => "PP-OCRv6_small_rec",
            Tier::Medium => "PP-OCRv6_medium_rec",
        }
    }

    pub fn det_size_mb(self) -> f32 {
        match self {
            Tier::Tiny => 1.7,
            Tier::Small => 9.5,
            Tier::Medium => 59.0,
        }
    }

    pub fn rec_size_mb(self) -> f32 {
        match self {
            Tier::Tiny => 4.3,
            Tier::Small => 20.0,
            Tier::Medium => 73.0,
        }
    }
}

/// Especificación de un modelo ONNX remoto (URL + checksum SHA-256).
pub struct ModelSpec {
    pub id: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
}

/// Manifesto: ONNX re-exportados "tract-friendly" del CDN del proyecto de
/// referencia. Checksums verificados el 2026-08-19 contra las descargas.
pub const MANIFEST: &[ModelSpec] = &[
    ModelSpec {
        id: "PP-OCRv6_medium_det",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_medium_det.onnx",
        sha256: "125ade0d59993f53c686fc5ce3a5a38f917297bd90b7cd0c4b1030a147e544c7",
    },
    ModelSpec {
        id: "PP-OCRv6_medium_rec",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_medium_rec.onnx",
        sha256: "ed577cfca4dfa8867261b483013ed02ccddfcf48b20e11f07ac44cfcc5a75d3f",
    },
    ModelSpec {
        id: "PP-OCRv6_tiny_det",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_tiny_det.onnx",
        sha256: "36f918a04075fa4002d8b4c4fdee4f5e5fdd282f6579510490f3a3e9e4cb96cb",
    },
    ModelSpec {
        id: "PP-OCRv6_tiny_rec",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_tiny_rec.onnx",
        sha256: "741f5f765e7321108a3df97c888e0f65f659e62f3adb1c944e04f0898a018f7f",
    },
    ModelSpec {
        id: "PP-OCRv6_small_det",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_small_det.onnx",
        sha256: "c00ff519e0fe30ffdc19763732773f53aa6b5eac61432ca03248d189800d76ee",
    },
    ModelSpec {
        id: "PP-OCRv6_small_rec",
        url: "https://cdn.drmhse.com/models/ocr-ppocrv6/v1/PP-OCRv6_small_rec.onnx",
        sha256: "a094aa198f9ddf29aa7f5d2dd1bc724e8dabb047ab78f401288cbbfbc62f687c",
    },
];

pub fn spec(id: &str) -> Result<&'static ModelSpec> {
    MANIFEST
        .iter()
        .find(|m| m.id == id)
        .with_context(|| format!("modelo desconocido '{id}' (no está en el manifiesto)"))
}

/// Directorio de caché de modelos.
pub fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("OCTXT_CACHE_DIR") {
        return PathBuf::from(d);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("octxt").join("models");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache").join("octxt").join("models");
    }
    PathBuf::from("octxt-models")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Devuelve la ruta local de un modelo, descargándolo (con verificación SHA-256)
/// si aún no está cacheado o si el checksum del archivo cacheado no coincide con
/// el manifest (p. ej. tras cambiar de fuente de pesos).
pub fn ensure(id: &str) -> Result<PathBuf> {
    let spec = spec(id)?;
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creando directorio de caché {}", dir.display()))?;
    let path = dir.join(format!("{}.onnx", spec.id));

    if path.exists() {
        let cached = std::fs::read(&path)
            .with_context(|| format!("leyendo {}", path.display()))?;
        if sha256_hex(&cached) == spec.sha256 {
            return Ok(path);
        }
        eprintln!(
            "  ! {}.onnx cacheado no coincide con el manifest; redescargando",
            spec.id
        );
    }

    eprintln!(
        "Descargando {} (primera ejecución, {:.0} MB)...",
        spec.id,
        size_mb_hint(spec.id)
    );
    let mut bytes = Vec::new();
    ureq::get(spec.url)
        .set("User-Agent", "octxt/0.1 (+https://github.com/octxt)")
        .call()
        .with_context(|| format!("GET {}", spec.url))?
        .into_reader()
        .read_to_end(&mut bytes)
        .with_context(|| format!("descargando {}", spec.url))?;

    let got = sha256_hex(&bytes);
    if got != spec.sha256 {
        bail!(
            "checksum inválido para '{id}': esperado {}, obtenido {}",
            spec.sha256,
            got
        );
    }

    let tmp = dir.join(format!("{}.tmp", spec.id));
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("escribiendo {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("finalizando {}", path.display()))?;
    eprintln!(
        "  ✓ {}.onnx cacheado ({} bytes, SHA-256 verificado)",
        spec.id,
        bytes.len()
    );
    Ok(path)
}

fn size_mb_hint(id: &str) -> f32 {
    match id {
        "PP-OCRv6_medium_det" => 59.0,
        "PP-OCRv6_medium_rec" => 73.0,
        "PP-OCRv6_tiny_det" => 1.7,
        "PP-OCRv6_tiny_rec" => 4.3,
        "PP-OCRv6_small_det" => 9.5,
        "PP-OCRv6_small_rec" => 20.0,
        _ => 0.0,
    }
}

/// Comprueba que los modelos del tier estén cacheados y devuelve sus rutas.
pub fn ensure_tier(tier: Tier) -> Result<(PathBuf, PathBuf)> {
    Ok((ensure(tier.det_id())?, ensure(tier.rec_id())?))
}

/// `true` si el modelo ya está cacheado.
pub fn is_cached(id: &str) -> bool {
    cache_dir().join(format!("{id}.onnx")).exists()
}

/// Verifica el checksum de un archivo cacheado. Devuelve `Ok` si coincide.
pub fn verify_cached(id: &str) -> Result<()> {
    let spec = spec(id)?;
    let path = cache_dir().join(format!("{}.onnx", spec.id));
    if !path.exists() {
        bail!("no está cacheado: {}", spec.id);
    }
    let bytes = std::fs::read(&path)
        .with_context(|| format!("leyendo {}", path.display()))?;
    let got = sha256_hex(&bytes);
    if got != spec.sha256 {
        bail!(
            "checksum inválido para '{}' en {}: esperado {}, obtenido {}",
            spec.id,
            path.display(),
            spec.sha256,
            got
        );
    }
    Ok(())
}
