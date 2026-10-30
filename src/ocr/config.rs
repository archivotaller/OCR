//! Parámetros del pipeline OCR (detección + normalización).
//!
//! Los valores por defecto corresponden a los publicados en `inference.yml`
//! de los modelos PP-OCRv6 (`DBPostProcess` / `NormalizeImage`).

/// Cómo se aplica el `limit_side_len` al redimensionar la imagen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitType {
    /// El lado más largo queda acotado por `limit_side_len`.
    Max,
    /// El lado más corto crece hasta `limit_side_len`.
    Min,
}

/// Parámetros de post-procesado de detección (DBPostProcess).
#[derive(Debug, Clone, Copy)]
pub struct DetParams {
    /// Umbral de binarización sobre el mapa de probabilidad.
    pub thresh: f32,
    /// Puntuación media mínima dentro de una caja para conservarla.
    pub box_thresh: f32,
    /// Ratio de expansión de la caja (unclip).
    pub unclip_ratio: f32,
    /// Lado de referencia para el redimensionado (múltiplo de 32).
    pub limit_side_len: u32,
    pub limit_type: LimitType,
    /// Cajas cuyo lado menor (px, en espacio de red) sea inferior a esto se descartan.
    pub min_size: f32,
    /// Tope de contornos candidatos considerados.
    pub max_candidates: usize,
    /// Si la red de detección emite logits y hay que aplicar sigmoide.
    pub apply_sigmoid: bool,
}

impl Default for DetParams {
    fn default() -> Self {
        DetParams {
            thresh: 0.2,
            box_thresh: 0.45,
            unclip_ratio: 1.4,
            limit_side_len: 960,
            limit_type: LimitType::Max,
            min_size: 3.0,
            max_candidates: 3000,
            apply_sigmoid: false,
        }
    }
}

/// Normalización estilo ImageNet (orden de canales acorde al `img_mode` del yml).
#[derive(Debug, Clone, Copy)]
pub struct Normalize {
    pub mean: [f32; 3],
    pub std: [f32; 3],
    pub scale: f32,
    /// `true` => se alimenta la red en orden BGR (por defecto en PP-OCR).
    pub bgr: bool,
}

impl Default for Normalize {
    fn default() -> Self {
        Normalize {
            mean: [0.485, 0.456, 0.406],
            std: [0.229, 0.224, 0.225],
            scale: 1.0 / 255.0,
            bgr: true,
        }
    }
}

/// Configuración completa del motor de detección.
#[derive(Debug, Clone, Copy)]
pub struct DetConfig {
    pub params: DetParams,
    pub normalize: Normalize,
}

impl Default for DetConfig {
    fn default() -> Self {
        DetConfig {
            params: DetParams::default(),
            normalize: Normalize::default(),
        }
    }
}
