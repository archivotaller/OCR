//! PDF buscable: capa de texto invisible, fuente Unicode y multipágina.

use octxt::ocr::engine::OcrLine;
use octxt::output::pdf::{searchable_pdf, searchable_pdf_pages, PdfPage};
use std::path::PathBuf;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("octxt-tests");
    std::fs::create_dir_all(&dir).expect("crear directorio temporal");
    dir.join(name)
}

fn page_image(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        w,
        h,
        image::Rgb([250, 250, 250]),
    ))
}

fn line(text: &str, x: f32, y: f32) -> OcrLine {
    OcrLine {
        points: [(x, y), (x + 100.0, y), (x + 100.0, y + 20.0), (x, y + 20.0)],
        text: text.to_string(),
        det_score: 0.9,
        rec_score: 0.8,
    }
}

#[test]
fn pdf_multipagina_tiene_fuente_unicode_y_dos_paginas() {
    let images = [page_image(120, 60), page_image(120, 60)];
    let lines_a = [line("Hola mundo", 5.0, 5.0), line("ñandú — 123", 5.0, 30.0)];
    let lines_b = [line("Second page / página 2", 5.0, 5.0)];

    let pages = [
        PdfPage {
            orig_bytes: None,
            image: &images[0],
            lines: &lines_a,
        },
        PdfPage {
            orig_bytes: None,
            image: &images[1],
            lines: &lines_b,
        },
    ];
    let out = temp_path("multipagina.pdf");
    searchable_pdf_pages(&pages, &out).expect("generar PDF multipágina");

    let bytes = std::fs::read(&out).expect("leer PDF");
    assert!(bytes.starts_with(b"%PDF-"), "cabecera PDF inválida");
    assert!(bytes.ends_with(b"%%EOF\n") || bytes.ends_with(b"%%EOF"));

    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Type0"), "debe usar una fuente Type0");
    assert!(text.contains("ToUnicode"), "debe incluir el CMap ToUnicode");
    assert!(
        text.contains("/Count 2"),
        "el árbol de páginas debe declarar 2 páginas"
    );

    let doc = lopdf::Document::load(&out).expect("lopdf debe poder abrir el PDF");
    assert_eq!(doc.get_pages().len(), 2, "el PDF debe tener 2 páginas");
}

#[test]
fn pdf_una_pagina_sigue_funcionando() {
    let img = page_image(80, 40);
    let lines = [line("test", 2.0, 2.0)];
    let out = temp_path("una_pagina.pdf");
    searchable_pdf(None, &img, &lines, &out).expect("generar PDF de una página");

    let doc = lopdf::Document::load(&out).expect("lopdf debe poder abrir el PDF");
    assert_eq!(doc.get_pages().len(), 1);
}
