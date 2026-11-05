//! Definición de la CLI y despacho de comandos.

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use octxt::font::FontSpec;
use octxt::image_io;
use octxt::input::pdf::PdfInput;
use octxt::models::{self, Tier};
use octxt::ocr::config::{DetConfig, DetParams};
use octxt::ocr::engine::{Engine, EngineConfig};
use octxt::output::{common::FileResult, json, txt};
use octxt::replace::{PageContent, ReplaceOptions, Rgb};
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

    /// Número de hilos de cómputo (por defecto: todos los núcleos lógicos).
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
    /// Genera un PDF buscable con capa de texto invisible; con --replace-text,
    /// un PDF con el texto original borrado y el del OCR visible y editable.
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
    /// Imágenes, PDF o directorios a procesar.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// Directorio de salida (si no se indica, junto a cada imagen).
    #[arg(long)]
    pub out_dir: Option<PathBuf>,

    /// Archivo PDF único (solo con una entrada).
    #[arg(long, conflicts_with = "out_dir")]
    pub out: Option<PathBuf>,

    /// Recorre directorios recursivamente.
    #[arg(long)]
    pub recursive: bool,

    /// Genera la variante con texto visible: borra el original y escribe
    /// encima el texto del OCR (buscable y editable).
    #[arg(long)]
    pub replace_text: bool,

    /// Cómo se elimina el texto original.
    #[arg(long, value_enum, default_value_t = EraseArg::Real)]
    pub erase_mode: EraseArg,

    /// Margen alrededor de cada caja, como fracción de su altura.
    #[arg(long, default_value_t = 0.3)]
    pub erase_margin: f32,

    /// Color de fondo fijo (`#rrggbb`) o `auto` (muestreado por caja).
    #[arg(long, default_value = "auto")]
    pub background: String,

    /// Color del texto fijo (`#rrggbb`) o `auto` (contraste con el fondo).
    #[arg(long, default_value = "auto")]
    pub text_color: String,

    /// Fuente TTF/OTF para latín (por defecto, Noto Sans incluida).
    #[arg(long)]
    pub font: Option<PathBuf>,

    /// Índice dentro de una colección TTC para `--font`.
    #[arg(long, default_value_t = 0)]
    pub font_index: u32,

    /// Fuente CJK explícita (por defecto se busca una en el sistema).
    #[arg(long)]
    pub font_cjk: Option<PathBuf>,

    /// Índice dentro de una colección TTC para `--font-cjk`.
    #[arg(long, default_value_t = 0)]
    pub font_cjk_index: u32,

    /// Usa la fuente incluida con serifa en lugar de la sans.
    #[arg(long)]
    pub font_serif: bool,

    /// Resolución de rasterizado de los PDF de entrada.
    #[arg(long, default_value_t = 300.0)]
    pub dpi: f32,

    /// Cajas que no se reemplazan, con formato `página:línea` (1-based).
    #[arg(long, value_delimiter = ',')]
    pub exclude: Vec<String>,
}

/// Modo de borrado en la CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EraseArg {
    Real,
    Visual,
}

impl From<EraseArg> for octxt::replace::EraseMode {
    fn from(v: EraseArg) -> Self {
        match v {
            EraseArg::Real => octxt::replace::EraseMode::Real,
            EraseArg::Visual => octxt::replace::EraseMode::Visual,
        }
    }
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
                models::verify_cached(id).with_context(|| format!("verificando {id}"))?;
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

// Los parámetros reflejan los flags globales de la CLI uno a uno.
#[allow(clippy::too_many_arguments)]
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
                eprintln!(
                    "aviso: ignorando '{}' (extensión no soportada)",
                    p.display()
                );
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
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("leyendo directorio {}", dir.display()))?
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
        std::fs::create_dir_all(dir).with_context(|| format!("creando {}", dir.display()))?;
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
    let files = expand_pdf_inputs(&a.paths, a.recursive)?;
    if files.is_empty() {
        bail!("no se encontraron imágenes ni PDF");
    }
    if a.out.is_some() && files.len() != 1 {
        bail!("--out solo es válido con una única entrada");
    }
    if let Some(dir) = &a.out_dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creando {}", dir.display()))?;
    }

    let opts = build_replace_options(a)?;

    for f in &files {
        let out_path = output_path(f, a);
        if a.replace_text {
            let (pages, original) = prepare_pages(engine, f, a)?;
            if pages.is_empty() {
                continue;
            }
            let plan = octxt::replace::build(&pages, &opts)?;
            report_plan(&plan);
            octxt::output::visible::write_visible_pdf(
                original.as_deref(),
                &pages,
                &plan,
                opts.mode,
                &out_path,
            )?;
            eprintln!(
                "✓ {} ({} líneas reemplazadas, {} omitidas, {} con overlay)",
                out_path.display(),
                plan.erased_lines(),
                plan.skipped_lines(),
                plan.overlay_pages()
            );
            continue;
        }

        if is_pdf(f) {
            bail!(
                "'{}' es un PDF: añade --replace-text para generar la versión con texto visible",
                f.display()
            );
        }

        let orig_bytes = std::fs::read(f).ok();
        let imgs = image_io::load_images(f)?;
        if imgs.is_empty() {
            continue;
        }

        // OCR de todas las páginas y un único PDF multipágina por archivo.
        let mut lines_per_page: Vec<Vec<octxt::ocr::engine::OcrLine>> =
            Vec::with_capacity(imgs.len());
        for (idx, img) in imgs.iter().enumerate() {
            let t0 = Instant::now();
            let lines = engine.ocr(img)?;
            eprintln!(
                "{}: página {}/{}: {} líneas en {} ms",
                f.display(),
                idx + 1,
                imgs.len(),
                lines.len(),
                t0.elapsed().as_millis()
            );
            lines_per_page.push(lines);
        }

        let pages: Vec<octxt::output::pdf::PdfPage<'_>> = imgs
            .iter()
            .enumerate()
            .map(|(idx, img)| octxt::output::pdf::PdfPage {
                orig_bytes: if idx == 0 {
                    orig_bytes.as_deref()
                } else {
                    None
                },
                image: img,
                lines: &lines_per_page[idx],
            })
            .collect();
        octxt::output::pdf::searchable_pdf_pages(&pages, &out_path)?;
        eprintln!("✓ {}", out_path.display());
    }
    Ok(())
}

/// Ruta de salida para una entrada (sufijo `.visible.pdf` en modo reemplazo).
fn output_path(f: &Path, a: &PdfArgs) -> PathBuf {
    if let Some(out) = &a.out {
        return out.clone();
    }
    let stem = f
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "salida".to_string());
    let name = if a.replace_text {
        format!("{stem}.visible.pdf")
    } else {
        format!("{stem}.pdf")
    };
    match &a.out_dir {
        Some(dir) => dir.join(name),
        None => f.with_file_name(name),
    }
}

fn build_replace_options(a: &PdfArgs) -> Result<ReplaceOptions> {
    let background = parse_color_arg("--background", &a.background)?;
    let text_color = parse_color_arg("--text-color", &a.text_color)?;
    let excluded = parse_excluded(&a.exclude)?;
    Ok(ReplaceOptions {
        mode: a.erase_mode.into(),
        margin: a.erase_margin,
        background,
        text_color,
        font: FontSpec {
            latin: a.font.clone(),
            latin_index: a.font_index,
            cjk_index: a.font_cjk_index,
            serif: a.font_serif,
            cjk: a.font_cjk.clone(),
            extra: Vec::new(),
        },
        dpi: a.dpi.max(36.0),
        excluded,
    })
}

fn parse_color_arg(flag: &str, value: &str) -> Result<Option<Rgb>> {
    if value.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    Rgb::parse(value)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("{flag}: color no válido '{value}' (usa #rrggbb o auto)"))
}

/// Convierte `página:línea` (1-based) en índices 0-based.
fn parse_excluded(specs: &[String]) -> Result<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    for spec in specs {
        let spec = spec.trim();
        if spec.is_empty() {
            continue;
        }
        let (p, l) = spec.split_once(':').ok_or_else(|| {
            anyhow::anyhow!("--exclude: formato esperado página:línea, no '{spec}'")
        })?;
        let page: usize = p
            .trim()
            .parse()
            .with_context(|| format!("--exclude: página no válida en '{spec}'"))?;
        let line: usize = l
            .trim()
            .parse()
            .with_context(|| format!("--exclude: línea no válida en '{spec}'"))?;
        if page == 0 || line == 0 {
            bail!("--exclude: la página y la línea empiezan en 1 ('{spec}')");
        }
        out.push((page - 1, line - 1));
    }
    Ok(out)
}

/// Rasteriza y hace OCR de una entrada (imágenes/TIFF o PDF).
fn prepare_pages(
    engine: &Engine,
    f: &Path,
    a: &PdfArgs,
) -> Result<(Vec<PageContent>, Option<PathBuf>)> {
    if is_pdf(f) {
        let input = PdfInput::open(f, None)?;
        let count = input.page_count()?;
        let mut pages = Vec::with_capacity(count);
        for idx in 0..count {
            let t0 = Instant::now();
            let raster = input.rasterize(idx, a.dpi)?;
            let lines = engine.ocr(&raster.image)?;
            let source_fonts = input.source_fonts(idx, &raster, &lines)?;
            eprintln!(
                "{}: página {}/{}: {} líneas en {} ms{}",
                f.display(),
                idx + 1,
                count,
                lines.len(),
                t0.elapsed().as_millis(),
                if raster.scanned { " (escaneada)" } else { "" }
            );
            pages.push(PageContent {
                image: raster.image,
                to_page: raster.to_page,
                width_pt: raster.width_pt,
                height_pt: raster.height_pt,
                rotation: raster.rotation,
                scanned: raster.scanned,
                from_pdf: true,
                source_fonts,
                lines,
            });
        }
        Ok((pages, Some(f.to_path_buf())))
    } else {
        let imgs = image_io::load_images(f)?;
        let total = imgs.len();
        let mut pages = Vec::with_capacity(total);
        for (idx, img) in imgs.into_iter().enumerate() {
            let t0 = Instant::now();
            let lines = engine.ocr(&img)?;
            eprintln!(
                "{}: página {}/{}: {} líneas en {} ms",
                f.display(),
                idx + 1,
                total,
                lines.len(),
                t0.elapsed().as_millis()
            );
            let mut page = PageContent::from_image(img);
            page.lines = lines;
            pages.push(page);
        }
        Ok((pages, None))
    }
}

fn report_plan(plan: &octxt::replace::Plan) {
    for page in &plan.pages {
        let kind = if page.overlay {
            "overlay (vectores conservados)"
        } else {
            "raster"
        };
        eprintln!(
            "  página {}: {} líneas reemplazadas, {} omitidas · {}",
            page.page + 1,
            page.lines.len(),
            page.skipped.len(),
            kind
        );
        for skip in &page.skipped {
            let preview: String = skip.text.chars().take(48).collect();
            eprintln!(
                "    línea {}: {} — «{}»",
                skip.line + 1,
                skip.reason,
                preview.trim()
            );
        }
        let mut fuentes: Vec<String> = page
            .lines
            .iter()
            .filter_map(|l| l.source.as_ref().map(|s| s.describe()))
            .collect();
        fuentes.sort();
        fuentes.dedup();
        if !fuentes.is_empty() {
            eprintln!("    fuente original: {}", fuentes.join(" · "));
        }
        if page.non_uniform {
            eprintln!("    aviso: el fondo no es uniforme; se usa el color dominante");
        }
    }
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

/// Expande entradas aceptando tanto imágenes como PDF.
fn expand_pdf_inputs(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            collect_dir_pdf(p, recursive, &mut out)?;
        } else if p.exists() {
            if !image_io::has_image_ext(p) && !is_pdf(p) {
                eprintln!(
                    "aviso: ignorando '{}' (extensión no soportada)",
                    p.display()
                );
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

fn collect_dir_pdf(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("leyendo directorio {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            if recursive {
                collect_dir_pdf(&path, true, out)?;
            }
        } else if ft.is_file() && (image_io::has_image_ext(&path) || is_pdf(&path)) {
            out.push(path);
        }
    }
    Ok(())
}
