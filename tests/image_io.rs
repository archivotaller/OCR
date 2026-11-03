//! Carga de imágenes: TIFF de 1 bit (escaneos), CMYK, 16 bits y multipágina.

use octxt::image_io::load_images;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn tiff_bilevel_se_lee_y_no_queda_en_blanco() {
    let pages = load_images(&fixture("eurotext.tif")).expect("cargar TIFF 1 bit");
    assert_eq!(pages.len(), 1);
    let img = pages[0].to_rgb8();
    assert_eq!((img.width(), img.height()), (1024, 800));

    let mut dark = 0usize;
    let mut light = 0usize;
    for px in img.pixels() {
        let [r, g, b] = px.0;
        let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        if luma < 32.0 {
            dark += 1;
        } else if luma > 224.0 {
            light += 1;
        }
    }
    let total = (img.width() * img.height()) as f32;
    assert!(
        dark as f32 / total > 0.01,
        "el TIFF debería tener texto oscuro (dark={dark})"
    );
    assert!(
        light as f32 / total > 0.5,
        "el TIFF debería tener fondo claro (light={light})"
    );
}

#[test]
fn tiff_cmyk_se_convierte_a_rgb() {
    let pages = load_images(&fixture("cmyk_2x2.tif")).expect("cargar TIFF CMYK");
    let img = pages[0].to_rgb8();
    assert_eq!((img.width(), img.height()), (2, 2));
    let got: Vec<[u8; 3]> = img.pixels().map(|p| p.0).collect();
    assert_eq!(
        got,
        vec![[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255],]
    );
}

#[test]
fn tiff_multipagina_devuelve_todas_las_paginas() {
    let pages = load_images(&fixture("multipage_2.tif")).expect("cargar TIFF multipágina");
    assert_eq!(pages.len(), 2);
    for (i, expected) in [(0usize, 255u8), (1, 0)] {
        let img = pages[i].to_rgb8();
        assert!(
            img.pixels().all(|p| p.0 == [expected; 3]),
            "página {i} debería ser uniforme {expected}"
        );
    }
}

#[test]
fn tiff_16_bits_usa_el_byte_alto() {
    let pages = load_images(&fixture("gray16.tif")).expect("cargar TIFF 16 bits");
    let img = pages[0].to_rgb8();
    assert_eq!((img.width(), img.height()), (16, 4));
    let first = img.get_pixel(0, 0).0[0];
    let last = img.get_pixel(15, 0).0[0];
    assert!(first <= 1, "primer píxel casi negro, fue {first}");
    assert!(last >= 254, "último píxel casi blanco, fue {last}");
}
