//! Definición de la CLI y despacho de comandos.

use octxt::image_io;
use octxt::models::{self, Tier};
use octxt::ocr::config::{DetConfig, DetParams};
use octxt::ocr::engine::{Engine, EngineConfig};
use octxt::output::{common::FileResult, json, txt};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "octxt",
    version,
    about = "Extracción de texto con PP-OCRv6 (tract, 100% Rust): texto, JSON y PDF buscable.",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Tier del modelo PP-OCRv6 (tiny = rápido, medium = más preciso).
    #[arg(long, value_enum, global = true, default_value = "medium")]
    pub tier: TierArg,

    /// Directorio de caché de modelos (por defecto %LOCALAPPDATA%\\octxt\\models).
    #[arg(long, global = true)]
    pub model_dir: Option<PathBuf>,

    /// Número de hilos (por defecto: núcleos físicos).
    #[arg(long, global = true)]
    pub threads: Option<usize>,

    /// Umbral de binarización de la detección (DBPostProcess thresh).
    #[arg(long, global = true, default_value_t = 0.2)]
    pub det_thresh: f32,

    /// Puntuación de caja mínima para conservarla.
    #[arg(long, global = true, default_value_t = 0.45)]
    pub box_thresh: f32,

    /// Ratio de expansión de caja (unclip).
    #[arg(long, global = true, default_value_t = 1.4)]
    pub unclip_ratio: f32,

    /// Lado de referencia del redimensionado de detección (múltiplo de 32).
    #[arg(long, global = true, default_value_t = 960)]
    pub limit_side_len: u32,

    /// Descarta líneas con confianza de reconocimiento inferior a este valor.
    #[arg(long, global = true, default_value_t = 0.3)]
    pub min_rec_score: f32,

    /// Reconoce palabra por palabra para recuperar espacios (más lento).
    #[arg(long, global = true)]
    pub split_words: bool,

    /// Tamaño de la caché de planes compilados de tract.
    #[arg(long, global = true, default_value_t = 16)]
    pub plan_cache: usize,

    /// Desactiva el pre-warm de planes al arrancar.
    #[arg(long, global = true)]
    pub no_prewarm: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Extrae el texto (stdout, archivo único o un .txt por imagen).
    Txt(TxtArgs),
    /// Genera un PDF buscable con capa de texto invisible.
    Pdf(PdfArgs),
    /// Lista las cajas detectadas con texto y confianza (JSON o TSV).
    Boxes(BoxesArgs),
    /// Descarga y cachea los modelos del tier elegido.
    Fetch,
    /// Verifica la integridad de los modelos cacheados.
    Check,
}

#[derive(Args)]
pub struct TxtArgs {
    /// Imágenes o directorios a procesar.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Escribe todo a un único archivo.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// Escribe un archivo .txt por imagen en este directorio.
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Salida en Markdown (cabecera por archivo).
    #[arg(long)]
    pub md: bool,

    /// Salida estructurada JSON.
    #[arg(long)]
    pub json: bool,

    /// Recorre directorios recursivamente.
    #[arg(long)]
    pub recursive: bool,
}

#[derive(Args)]
pub struct PdfArgs {
    /// Imágenes o directorios a procesar.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Directorio de salida (si no se indica, junto a cada imagen).
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Archivo PDF único (solo con una imagen de entrada).
    #[arg(long, conflicts_with = "out_dir")]
    pub out: Option<PathBuf>,

    /// Recorre directorios recursivamente.
    #[arg(long)]
    pub recursive: bool,
}

#[derive(Args)]
pub struct BoxesArgs {
    /// Imágenes o directorios a procesar.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Salida en JSON (por defecto es TSV).
    #[arg(long)]
    pub json: bool,

    /// Recorre directorios recursivamente.
    #[arg(long)]
    pub recursive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TierArg {
    Tiny,
    Small,
    Medium,
}

impl From<TierArg> for Tier {
    fn from(t: TierArg) -> Self {
        match t {
            TierArg::Tiny => Tier::Tiny,
            TierArg::Small => Tier::Small,
            TierArg::Medium => Tier::Medium,
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let Cli {
        command: cmd,
        tier: tier_arg,
        model_dir,
        threads,
        det_thresh,
        box_thresh,
        unclip_ratio,
        limit_side_len,
        min_rec_score,
        split_words,
        plan_cache,
        no_prewarm,
    } = cli;
    let tier = Tier::from(tier_arg);
    match cmd {
        Command::Fetch => {
            let (d, r) = models::ensure_tier(tier)?;
            println!("{} -> {}", tier.det_id(), d.display());
            println!("{} -> {}", tier.rec_id(), r.display());
            Ok(())
        }
        Command::Check => {
            for id in [tier.det_id(), tier.rec_id()] {
                models::verify_cached(id)
                    .with_context(|| format!("verificando {id}"))?;
                println!("✓ {id}: ok");
            }
            Ok(())
        }
        Command::Txt(a) => {
            let engine = build_engine(
                tier,
                det_thresh,
                box_thresh,
                unclip_ratio,
                limit_side_len,
                min_rec_score,
                split_words,
                plan_cache,
                threads,
                no_prewarm,
                model_dir.as_deref(),
            )?;
            run_txt(&engine, &a)
        }
        Command::Boxes(a) => {
            let engine = build_engine(
                tier,
                det_thresh,
                box_thresh,
                unclip_ratio,
                limit_side_len,
                min_rec_score,
                split_words,
                plan_cache,
                threads,
                no_prewarm,
                model_dir.as_deref(),
            )?;
            run_boxes(&engine, &a)
        }
        Command::Pdf(a) => {
            let engine = build_engine(
                tier,
                det_thresh,
                box_thresh,
                unclip_ratio,
                limit_side_len,
                min_rec_score,
                split_words,
                plan_cache,
                threads,
                no_prewarm,
                model_dir.as_deref(),
            )?;
            run_pdf(&engine, &a)
        }
    }
}

fn build_engine(
    tier: Tier,
    det_thresh: f32,
    box_thresh: f32,
    unclip_ratio: f32,
    limit_side_len: u32,
    min_rec_score: f32,
    split_words: bool,
    plan_cache: usize,
    threads: Option<usize>,
    no_prewarm: bool,
    model_dir: Option<&Path>,
) -> Result<Engine> {
    let det = DetConfig {
        params: DetParams {
            thresh: det_thresh,
            box_thresh,
            unclip_ratio,
            limit_side_len,
            ..Default::default()
        },
        normalize: Default::default(),
    };
    let cfg = EngineConfig {
        tier,
        det,
        min_rec_score,
        split_words,
        plan_cache_cap: plan_cache,
        threads: threads.unwrap_or(0),
        prewarm: !no_prewarm,
    };
    Engine::load(&cfg, model_dir)
}

/// Expande rutas (archivos o directorios) a la lista de imágenes a procesar.
fn expand_inputs(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            collect_dir(p, recursive, &mut out)?;
        } else if p.exists() {
            if !image_io::has_image_ext(p) {
                eprintln!("aviso: ignorando '{}' (extensión no soportada)", p.display());
                continue;
            }
            out.push(p.clone());
        } else {
            bail!("no existe: {}", p.display());
        }
    }
    out.sort();
    Ok(out)
}

fn collect_dir(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .with_context(|| format!("leyendo directorio {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if recursive {
                collect_dir(&path, true, out)?;
            }
        } else if ft.is_file() && image_io::has_image_ext(&path) {
            out.push(path);
        }
    }
    Ok(())
}

/// Ejecuta OCR sobre todas las imágenes y devuelve resultados en orden.
fn ocr_all(engine: &Engine, files: &[PathBuf]) -> Result<Vec<FileResult>> {
    if files.is_empty() {
        bail!("no se encontraron imágenes");
    }
    let mut results = Vec::new();
    for f in files {
        let imgs = image_io::load_images(f)?;
        for img in imgs {
            let t0 = Instant::now();
            let lines = engine.ocr(&img)?;
            results.push(FileResult {
                file: f.display().to_string(),
                width: img.width(),
                height: img.height(),
                elapsed_ms: t0.elapsed().as_millis(),
                lines,
            });
        }
    }
    Ok(results)
}

fn run_txt(engine: &Engine, a: &TxtArgs) -> Result<()> {
    let files = expand_inputs(&a.paths, a.recursive)?;
    let results = ocr_all(engine, &files)?;

    if a.json {
        let out: Vec<json::FileJson> = results.iter().map(json::FileJson::from_result).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if let Some(dir) = &a.out_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creando {}", dir.display()))?;
        for r in &results {
            let stem = Path::new(&r.file)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "imagen".to_string());
            let out_path = dir.join(format!("{stem}.txt"));
            let body = if a.md {
                txt::markdown(&r.file, &r.lines)
            } else {
                let mut s = txt::plain(&r.lines);
                s.push('\n');
                s
            };
            std::fs::write(&out_path, body)
                .with_context(|| format!("escribiendo {}", out_path.display()))?;
            eprintln!("✓ {}", out_path.display());
        }
        return Ok(());
    }

    if let Some(out_path) = &a.out {
        let mut body = String::new();
        for r in &results {
            if a.md {
                body.push_str(&txt::markdown(&r.file, &r.lines));
            } else {
                body.push_str(&txt::plain(&r.lines));
                body.push('\n');
            }
            body.push('\n');
        }
        std::fs::write(out_path, body)
            .with_context(|| format!("escribiendo {}", out_path.display()))?;
        return Ok(());
    }

    // stdout
    for (i, r) in results.iter().enumerate() {
        if a.md {
            print!("{}", txt::markdown(&r.file, &r.lines));
        } else {
            print!("{}", txt::plain(&r.lines));
        }
        if i + 1 < results.len() {
            println!();
        }
    }
    Ok(())
}

fn run_boxes(engine: &Engine, a: &BoxesArgs) -> Result<()> {
    let files = expand_inputs(&a.paths, a.recursive)?;
    let results = ocr_all(engine, &files)?;

    if a.json {
        let out: Vec<json::FileJson> = results.iter().map(json::FileJson::from_result).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // TSV: file<TAB>x1,y1 x2,y2 ... x4,y4<TAB>det<TAB>rec<TAB>text
    for r in &results {
        for l in &r.lines {
            let quad = l
                .points
                .iter()
                .map(|(x, y)| format!("{x:.1},{y:.1}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{}\t{}\t{:.3}\t{:.3}\t{}",
                r.file, quad, l.det_score, l.rec_score, l.text
            );
        }
    }
    Ok(())
}

fn run_pdf(engine: &Engine, a: &PdfArgs) -> Result<()> {
    let files = expand_inputs(&a.paths, a.recursive)?;
    if files.is_empty() {
        bail!("no se encontraron imágenes");
    }
    if a.out.is_some() && files.len() != 1 {
        bail!("--out solo es válido con una única imagen de entrada");
    }
    if let Some(dir) = &a.out_dir {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creando {}", dir.display()))?;
    }

    for f in &files {
        let orig_bytes = std::fs::read(f).ok();
        let imgs = image_io::load_images(f)?;
        for (idx, img) in imgs.iter().enumerate() {
            let t0 = Instant::now();
            let lines = engine.ocr(img)?;
            eprintln!(
                "{}: {} líneas en {} ms",
                f.display(),
                lines.len(),
                t0.elapsed().as_millis()
            );

            let out_path = if let Some(out) = &a.out {
                out.clone()
            } else if let Some(dir) = &a.out_dir {
                let stem = f
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "imagen".to_string());
                if imgs.len() > 1 {
                    dir.join(format!("{stem}_p{}.pdf", idx + 1))
                } else {
                    dir.join(format!("{stem}.pdf"))
                }
            } else {
                let stem = f
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "imagen".to_string());
                f.with_file_name(format!("{stem}.pdf"))
            };

            let bytes_for_pdf = if idx == 0 { orig_bytes.as_deref() } else { None };
            octxt::output::pdf::searchable_pdf(bytes_for_pdf, img, &lines, &out_path)?;
            eprintln!("✓ {}", out_path.display());
        }
    }
    Ok(())
}
