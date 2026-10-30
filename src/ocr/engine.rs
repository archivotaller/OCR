//! Orquestación del pipeline OCR: detección -> orden de lectura ->
//! reconocimiento en paralelo. Basado en `gradan-hash/paddle-paddle-v6-ocr-rust`
//! (Apache-2.0), simplificado para uso como librería/CLI con un solo tier.

use crate::dict;
use crate::models::{self, Tier};
use crate::ocr::config::DetConfig;
use crate::ocr::geometry::Pt;
use crate::ocr::postprocess::{db_postprocess, DetectedBox};
use crate::ocr::preprocess::preprocess;
use crate::ocr::recognize::{
    ctc_classes_ok, rec_preprocess, rectify_crop, split_word_crops, width_buckets, Charset,
    REC_HEIGHT,
};
use anyhow::{Context, Result};
use image::DynamicImage;
use rayon::prelude::*;
use std::collections::{HashMap, VecDeque};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use tract_onnx::prelude::*;

type Runnable =
    RunnableModel<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

/// Un modelo parseado + caché LRU acotada de planes compilados (por tamaño).
struct Compiled {
    infer: InferenceModel,
    cache: Mutex<PlanCache>,
    label: String,
}

struct PlanCache {
    map: HashMap<(u32, u32), Arc<Runnable>>,
    order: VecDeque<(u32, u32)>,
    cap: usize,
}

impl PlanCache {
    fn new(cap: usize) -> Self {
        PlanCache {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    fn touch(&mut self, key: (u32, u32)) {
        if let Some(pos) = self.order.iter().position(|k| *k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    fn insert(&mut self, key: (u32, u32), plan: Arc<Runnable>) {
        self.map.insert(key, plan);
        self.touch(key);
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

impl Compiled {
    fn from_bytes(label: &str, onnx: &[u8], cap: usize) -> Result<Self> {
        let infer = tract_onnx::onnx()
            .model_for_read(&mut Cursor::new(onnx))
            .with_context(|| format!("leyendo onnx de {label}"))?;
        Ok(Compiled {
            infer,
            cache: Mutex::new(PlanCache::new(cap)),
            label: label.to_string(),
        })
    }

    fn plan_for(&self, h: u32, w: u32) -> Result<Arc<Runnable>> {
        let mut cache = self.cache.lock().expect("plan cache poisoned");
        if let Some(p) = cache.map.get(&(h, w)).cloned() {
            cache.touch((h, w));
            return Ok(p);
        }
        let plan = self
            .infer
            .clone()
            .with_input_fact(0, f32::fact([1, 3, h as usize, w as usize]).into())
            .context("fijando fact de entrada")?
            .into_optimized()
            .context("optimizando")?
            .into_runnable()
            .context("compilando plan")?;
        let plan = Arc::new(plan);
        cache.insert((h, w), plan.clone());
        Ok(plan)
    }
}

/// Una región de texto reconocida.
#[derive(Debug, Clone)]
pub struct OcrLine {
    /// Cuadrilátero en píxeles de la imagen original (tl, tr, br, bl).
    pub points: [Pt; 4],
    pub text: String,
    pub det_score: f32,
    pub rec_score: f32,
}

/// Configuración del motor.
pub struct EngineConfig {
    pub tier: Tier,
    pub det: DetConfig,
    pub min_rec_score: f32,
    /// Recortar y reconocer por palabra para recuperar espacios (más lento).
    pub split_words: bool,
    pub plan_cache_cap: usize,
    pub threads: usize,
    /// Compilar por adelantado los planes de reconocimiento (anchos fijos).
    pub prewarm: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            tier: Tier::Medium,
            det: DetConfig::default(),
            min_rec_score: 0.3,
            split_words: false,
            plan_cache_cap: 16,
            threads: 0, // auto
            prewarm: true,
        }
    }
}

pub struct Engine {
    det_cfg: DetConfig,
    det: Compiled,
    rec: Compiled,
    charset: Charset,
    min_rec_score: f32,
    split_words: bool,
}

impl Engine {
    /// Carga los modelos del tier (descargándolos si hace falta) y prepara el
    /// motor. `model_dir` opcional sobreescribe la caché por defecto.
    pub fn load(cfg: &EngineConfig, model_dir: Option<&std::path::Path>) -> Result<Self> {
        if cfg.threads > 0 {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(cfg.threads)
                .build()
                .context("creando pool rayon")?;
            let _ = pool.install(|| {});
        }
        if let Some(dir) = model_dir {
            std::env::set_var("OCTXT_CACHE_DIR", dir);
        }
        let (det_path, rec_path) = models::ensure_tier(cfg.tier)?;
        let det_onnx = std::fs::read(&det_path)
            .with_context(|| format!("leyendo {}", det_path.display()))?;
        let rec_onnx = std::fs::read(&rec_path)
            .with_context(|| format!("leyendo {}", rec_path.display()))?;

        let det = Compiled::from_bytes(cfg.tier.det_id(), &det_onnx, cfg.plan_cache_cap)?;
        let rec = Compiled::from_bytes(cfg.tier.rec_id(), &rec_onnx, cfg.plan_cache_cap)?;
        let charset = Charset::from_dict(dict::charset_for(cfg.tier));

        let engine = Engine {
            det_cfg: cfg.det,
            det,
            rec,
            charset,
            min_rec_score: cfg.min_rec_score,
            split_words: cfg.split_words,
        };
        if cfg.prewarm {
            engine.prewarm();
        }
        Ok(engine)
    }

    /// Compila por adelantado los planes de reconocimiento de ancho fijo.
    pub fn prewarm(&self) {
        for &w in width_buckets() {
            if let Err(e) = self.rec.plan_for(REC_HEIGHT, w) {
                eprintln!("aviso: prewarm de rec w={w} falló: {e:#}");
            }
        }
    }

    /// Detección únicamente (cajas + puntuaciones).
    pub fn detect(&self, img: &DynamicImage) -> Result<Vec<DetectedBox>> {
        let pre = preprocess(img, &self.det_cfg);
        let (net_w, net_h) = (pre.resize_w as usize, pre.resize_h as usize);

        let plan = self.det.plan_for(pre.resize_h, pre.resize_w)?;
        let result = plan
            .run(tvec!(pre.tensor.clone().into()))
            .context("inferencia de detección falló")?;
        let view = result[0]
            .to_array_view::<f32>()
            .context("salida de det no es f32")?;
        let prob: Vec<f32> = view.iter().cloned().collect();
        anyhow::ensure!(
            prob.len() == net_w * net_h,
            "tamaño inesperado de salida de det: {} != {}",
            prob.len(),
            net_w * net_h
        );
        Ok(db_postprocess(&prob, net_w, net_h, &pre, &self.det_cfg.params))
    }

    fn recognize_strip_raw(&self, line: &image::RgbImage) -> Result<(String, f32)> {
        let (tensor, pad_w) = rec_preprocess(line);
        let plan = self.rec.plan_for(REC_HEIGHT, pad_w)?;
        let result = plan
            .run(tvec!(tensor.into()))
            .context("inferencia de reconocimiento falló")?;
        let view = result[0]
            .to_array_view::<f32>()
            .context("salida de rec no es f32")?;
        let shape = view.shape();
        anyhow::ensure!(shape.len() == 3, "salida de rec con rango != 3");
        let (t, c) = (shape[1], shape[2]);
        ctc_classes_ok(&self.charset, c)?;
        let probs: Vec<f32> = view.iter().cloned().collect();
        Ok(self.charset.ctc_decode(&probs, t, c))
    }

    fn recognize_strip(&self, line: &image::RgbImage) -> Result<(String, f32)> {
        let (whole_text, whole_score) = self.recognize_strip_raw(line)?;
        if !self.split_words || whole_text.contains(' ') {
            return Ok((whole_text, whole_score));
        }

        let word_crops = split_word_crops(line);
        if word_crops.is_empty() {
            return Ok((whole_text, whole_score));
        }

        let mut words = Vec::with_capacity(word_crops.len());
        let mut weighted_score = 0.0f32;
        let mut total_chars = 0usize;
        for crop in &word_crops {
            let (word, score) = self.recognize_strip_raw(crop)?;
            let word = word.trim().to_string();
            if word.is_empty() {
                return Ok((whole_text, whole_score));
            }
            let chars = word.chars().count().max(1);
            weighted_score += score * chars as f32;
            total_chars += chars;
            words.push(word);
        }

        let segmented = words.join(" ");
        let segmented_compact: String = segmented.chars().filter(|c| !c.is_whitespace()).collect();
        let whole_compact: String = whole_text.chars().filter(|c| !c.is_whitespace()).collect();
        let segmented_score = weighted_score / total_chars.max(1) as f32;

        if segmented_compact.len() >= whole_compact.len().saturating_sub(2)
            && segmented_score + 0.08 >= whole_score
        {
            Ok((segmented, segmented_score))
        } else {
            Ok((whole_text, whole_score))
        }
    }

    /// Pipeline completo sobre una imagen. Devuelve las líneas en orden de
    /// lectura (arriba->abajo, izquierda->derecha).
    pub fn ocr(&self, img: &DynamicImage) -> Result<Vec<OcrLine>> {
        let mut boxes = self.detect(img)?;
        // Orden de lectura: arriba->abajo, luego izquierda->derecha,
        // agrupando filas a ~10px.
        boxes.sort_by(|a, b| {
            let ay = a.points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
            let by = b.points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
            let ax = a.points.iter().map(|p| p.0).fold(f32::MAX, f32::min);
            let bx = b.points.iter().map(|p| p.0).fold(f32::MAX, f32::min);
            if (ay - by).abs() <= 10.0 {
                ax.partial_cmp(&bx).unwrap()
            } else {
                ay.partial_cmp(&by).unwrap()
            }
        });

        let src = img.to_rgb8();
        // El reconocimiento es por-caja e independiente: en paralelo.
        let lines: Vec<OcrLine> = boxes
            .par_iter()
            .map(|b| -> Result<Option<OcrLine>> {
                let crop = rectify_crop(&src, &b.points, false);
                let (text, rec_score) = self.recognize_strip(&crop)?;
                Ok(if text.is_empty() || rec_score < self.min_rec_score {
                    None
                } else {
                    Some(OcrLine {
                        points: b.points,
                        text,
                        det_score: b.score,
                        rec_score,
                    })
                })
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();

        Ok(lines)
    }
}
