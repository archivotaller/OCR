//! Test E2E real: carga un TIFF de 1 bit descargado y ejecuta el pipeline OCR
//! completo con los modelos PP-OCRv6 cacheados.
//!
//! El tier se elige con `OCTXT_TEST_TIER` (tiny | small | medium). Por defecto
//! `tiny`, que es el más rápido y ya produce texto reconocible en el fixture.

use octxt::image_io::load_images;
use octxt::models::Tier;
use octxt::ocr::engine::{Engine, EngineConfig};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn tier() -> Tier {
    match std::env::var("OCTXT_TEST_TIER")
        .unwrap_or_else(|_| "tiny".into())
        .to_lowercase()
        .as_str()
    {
        "tiny" => Tier::Tiny,
        "small" => Tier::Small,
        "medium" => Tier::Medium,
        other => panic!("OCTXT_TEST_TIER desconocido: {other} (tiny|small|medium)"),
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn engine(threads: usize) -> Engine {
    let cfg = EngineConfig {
        tier: tier(),
        threads,
        ..Default::default()
    };
    Engine::load(&cfg, None).expect("cargar modelos y compilar el motor")
}

#[test]
fn ocr_real_sobre_tiff_bilevel() {
    let engine = engine(0);
    let pages = load_images(&fixture("eurotext.tif")).expect("cargar eurotext.tif");
    assert_eq!(pages.len(), 1);
    let img = &pages[0];
    let (w, h) = (img.width() as f32, img.height() as f32);

    let t0 = Instant::now();
    let lines = engine.ocr(img).expect("ejecutar OCR");
    let elapsed = t0.elapsed();

    assert!(
        lines.len() >= 8,
        "se esperaban varias líneas de texto, se obtuvieron {}",
        lines.len()
    );

    let all = lines
        .iter()
        .map(|l| l.text.to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in ["fuchs", "renard", "zorro"] {
        assert!(
            all.contains(expected),
            "el texto OCR debería contener {expected:?}; obtenido:\n{all}"
        );
    }

    for line in &lines {
        assert!(
            !line.text.trim().is_empty(),
            "no debería haber líneas vacías en el resultado"
        );
        assert!(
            (0.0..=1.0).contains(&line.det_score),
            "det_score fuera de rango: {}",
            line.det_score
        );
        assert!(
            (0.0..=1.0).contains(&line.rec_score),
            "rec_score fuera de rango: {}",
            line.rec_score
        );
        for (x, y) in line.points {
            assert!(
                (-2.0..=w + 2.0).contains(&x) && (-2.0..=h + 2.0).contains(&y),
                "punto ({x}, {y}) fuera de la imagen {w}x{h}"
            );
        }
    }

    // Orden de lectura: la primera caja debe estar por encima de la última.
    let first_y = lines[0].points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let last_y = lines
        .last()
        .unwrap()
        .points
        .iter()
        .map(|p| p.1)
        .fold(f32::MAX, f32::min);
    assert!(
        first_y < last_y,
        "las líneas deberían venir en orden de lectura (y={first_y} vs {last_y})"
    );

    eprintln!(
        "OCR {}: {} líneas en {:.2} s",
        tier().as_str(),
        lines.len(),
        elapsed.as_secs_f32()
    );
}

#[test]
fn threads_configura_el_pool_de_rayon() {
    let engine = engine(2);
    assert_eq!(
        engine.pool_threads(),
        2,
        "`--threads 2` debe crear un pool de exactamente 2 hilos"
    );
}
