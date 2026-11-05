//! Salida PDF con el texto reemplazado: borrado selectivo, conservación del
//! contenido original y fuentes incrustadas.

use image::{DynamicImage, Rgb, RgbImage};
use lopdf::{dictionary, Document, Object, Stream};
use octxt::geom::quad_bounds;
use octxt::input::pdf::PdfInput;
use octxt::ocr::engine::OcrLine;
use octxt::output::visible::write_visible_pdf;
use octxt::replace::{build, EraseMode, PageContent, ReplaceOptions};
use std::path::{Path, PathBuf};

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("octxt-tests");
    std::fs::create_dir_all(&dir).expect("crear directorio temporal");
    dir.join(name)
}

fn line(text: &str, quad: [[f32; 2]; 4]) -> OcrLine {
    OcrLine {
        points: [
            (quad[0][0], quad[0][1]),
            (quad[1][0], quad[1][1]),
            (quad[2][0], quad[2][1]),
            (quad[3][0], quad[3][1]),
        ],
        text: text.to_string(),
        det_score: 0.9,
        rec_score: 0.8,
    }
}

/// Imagen con franjas de color y una banda de "texto" en el centro.
fn patched_image() -> RgbImage {
    let mut img = RgbImage::from_pixel(200, 120, Rgb([200, 220, 240]));
    for y in 0..120 {
        for x in 0..200 {
            if y < 30 {
                img.put_pixel(x, y, Rgb([240, 240, 240]));
            } else if y > 90 {
                img.put_pixel(x, y, Rgb([180, 200, 220]));
            }
        }
    }
    for y in 50..70 {
        for x in 20..180 {
            img.put_pixel(x, y, Rgb([10, 10, 10]));
        }
    }
    img
}

/// Extrae la única imagen incrustada del PDF (bytes RGB sin comprimir).
fn embedded_image(path: &Path) -> RgbImage {
    let doc = Document::load(path).expect("abrir el PDF generado");
    for object in doc.objects.values() {
        let Ok(stream) = object.as_stream() else {
            continue;
        };
        let is_image = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|o| o.as_name().ok())
            == Some(b"Image".as_slice());
        if is_image {
            let width = stream
                .dict
                .get(b"Width")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .expect("ancho de la imagen") as u32;
            let height = stream
                .dict
                .get(b"Height")
                .ok()
                .and_then(|o| o.as_i64().ok())
                .expect("alto de la imagen") as u32;
            let data = stream
                .decompressed_content()
                .expect("descomprimir la imagen");
            return RgbImage::from_raw(width, height, data).expect("píxeles RGB");
        }
    }
    panic!("el PDF no contiene ninguna imagen");
}

#[test]
fn raster_borra_dentro_de_las_cajas_y_preserva_el_resto() {
    let original = patched_image();
    let quad = [[20.0, 50.0], [180.0, 50.0], [180.0, 70.0], [20.0, 70.0]];
    let mut page = PageContent::from_image(DynamicImage::ImageRgb8(original.clone()));
    page.lines.push(line("texto negro", quad));

    let pages = vec![page];
    let plan = build(&pages, &ReplaceOptions::default()).expect("plan");
    assert_eq!(plan.pages[0].lines.len(), 1);
    let out = temp_path("visible-raster.pdf");
    write_visible_pdf(None, &pages, &plan, EraseMode::Real, &out).expect("escribir el PDF");

    let got = embedded_image(&out);
    assert_eq!(got.dimensions(), original.dimensions());

    let (x0, _y0, _x1, y1) = quad_bounds(&plan.pages[0].lines[0].erase_px);
    // Fuera del parche cada píxel queda exactamente igual.
    for (x, y) in [
        (0u32, 0u32),
        (10, 10),
        (199, 5),
        (5, 60),
        (199, 60),
        (100, 115),
    ] {
        assert_eq!(
            got.get_pixel(x, y),
            original.get_pixel(x, y),
            "píxel modificado fuera de la caja en ({x}, {y})"
        );
    }
    // Dentro del parche se pinta el fondo muestreado (sin restos del texto).
    let inside = got.get_pixel(x0 as u32 + 1, y1 as u32 - 2);
    assert_eq!(
        inside,
        &Rgb([200, 220, 240]),
        "fondo inesperado: {inside:?}"
    );

    let raw = std::fs::read(&out).expect("leer el PDF");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Type0"), "falta la fuente Type0");
    assert!(text.contains("CIDFontType2"), "falta el CIDFontType2");
    assert!(text.contains("FontFile2"), "falta el programa de fuente");
    assert!(text.contains("ToUnicode"), "falta el CMap ToUnicode");
}

#[test]
fn modo_visual_deja_los_pixeles_intactos_y_tapa_con_un_rectangulo() {
    let original = patched_image();
    let quad = [[20.0, 50.0], [180.0, 50.0], [180.0, 70.0], [20.0, 70.0]];
    let mut page = PageContent::from_image(DynamicImage::ImageRgb8(original.clone()));
    page.lines.push(line("texto negro", quad));

    let pages = vec![page];
    let opts = ReplaceOptions {
        mode: EraseMode::Visual,
        ..Default::default()
    };
    let plan = build(&pages, &opts).expect("plan");
    let out = temp_path("visible-overlay-raster.pdf");
    write_visible_pdf(None, &pages, &plan, EraseMode::Visual, &out).expect("escribir");

    // La imagen incrustada sigue siendo el original, píxel a píxel.
    let got = embedded_image(&out);
    assert_eq!(got.as_raw(), original.as_raw());

    let doc = Document::load(&out).expect("abrir el PDF");
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_page_content(page_id);
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("h f"), "falta el rectángulo opaco: {text}");
    assert!(text.contains("Tj"), "falta el texto nuevo");
}

/// PDF vectorial mínimo: un rectángulo azul y una línea de texto Helvetica.
fn vector_pdf(path: &Path) {
    vector_pdf_rotated(path, 0);
}

fn vector_pdf_rotated(path: &Path, rotate: i64) {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "Encoding" => "WinAnsiEncoding",
    });
    let content = b"q\n0 0 1 RG\n2 w\n10 10 100 50 re S\nQ\nq\n0 0 0 rg\nBT\n/F1 18 Tf\n20 70 Td\n(Hola mundo) Tj\nET\nQ\n";
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
    let mut page_dict = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(100),
        ],
        "Contents" => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        },
    };
    if rotate != 0 {
        page_dict.set("Rotate", Object::Integer(rotate));
    }
    let page_id = doc.add_object(page_dict);
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.save(path).expect("generar el PDF vectorial");
}

#[test]
fn vectorial_conserva_el_contenido_y_superpone_el_texto() {
    let src = temp_path("visible-vector-src.pdf");
    vector_pdf(&src);

    let input = PdfInput::open(&src, None).expect("abrir el PDF de prueba");
    let raster = input.rasterize(0, 72.0).expect("rasterizar");
    assert!(!raster.scanned, "la página de prueba es vectorial");
    let lines = vec![line(
        "Hola mundo",
        [[15.0, 5.0], [150.0, 5.0], [150.0, 45.0], [15.0, 45.0]],
    )];
    let source_fonts = input
        .source_fonts(0, &raster, &lines)
        .expect("fuentes de origen");
    assert!(
        source_fonts[0].is_some(),
        "debería detectar el texto real del PDF"
    );

    let pages = vec![PageContent {
        image: raster.image,
        to_page: raster.to_page,
        width_pt: raster.width_pt,
        height_pt: raster.height_pt,
        rotation: raster.rotation,
        scanned: raster.scanned,
        from_pdf: true,
        source_fonts,
        lines,
    }];
    let plan = build(&pages, &ReplaceOptions::default()).expect("plan");
    assert!(
        plan.pages[0].overlay,
        "una página vectorial debe ir en overlay"
    );
    let planned = &plan.pages[0].lines[0];

    // La caja de borrado cubre la caja real del texto y la línea base es la
    // del original, no la estimación del detector.
    let source = planned.source.as_ref().expect("fuente de origen");
    let bbox = source.bbox.expect("caja real del texto");
    let (ex0, ey0, ex1, ey1) = quad_bounds(&planned.erase_pt);
    assert!(ex0 <= bbox.0 && ey0 <= bbox.1 && ex1 >= bbox.2 && ey1 >= bbox.3);
    assert_eq!(source.baseline.map(|b| b.0), Some(planned.baseline.0));
    assert!(
        (planned.size_pt - 18.0).abs() < 1.0,
        "tamaño {}",
        planned.size_pt
    );
    assert!(planned.baseline.1 >= bbox.1 && planned.baseline.1 <= bbox.3);

    let out = temp_path("visible-vector.pdf");
    write_visible_pdf(Some(&src), &pages, &plan, EraseMode::Real, &out).expect("escribir");

    let doc = Document::load(&out).expect("abrir la salida");
    assert_eq!(doc.get_pages().len(), 1);
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_page_content(page_id);
    let text = String::from_utf8_lossy(&content);
    // El dibujo y el texto originales siguen en la página...
    assert!(
        text.contains("re"),
        "se perdió el rectángulo vectorial: {text}"
    );
    assert!(text.contains("(Hola mundo)"), "se perdió el texto original");
    // ...y se añaden el parche y la capa de texto nueva.
    assert!(text.contains("rg"), "falta el relleno del parche");
    assert!(text.contains("Tj"), "falta el texto nuevo");
    assert!(
        text.matches("BT").count() >= 2,
        "falta el bloque de texto nuevo"
    );

    let raw = std::fs::read(&out).expect("leer la salida");
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("Type0"), "falta la fuente incrustada");
    assert!(raw.contains("FontFile2"), "falta el programa de fuente");
}

#[test]
fn pdf_rotado_90_conserva_la_rotacion_y_fuerza_overlay() {
    let src = temp_path("visible-vector-rot90-src.pdf");
    vector_pdf_rotated(&src, 90);

    let input = PdfInput::open(&src, None).expect("abrir el PDF de prueba");
    let raster = input.rasterize(0, 72.0).expect("rasterizar");
    assert_eq!(raster.rotation, 90);
    assert!(
        !raster.scanned,
        "una página rotada no se puede reconstruir desde el raster"
    );

    // La caja del texto se expresa en puntos de página y se proyecta al raster
    // (que PDFium gira) para construir la línea del OCR.
    let inv = raster.to_page.inverse().expect("afín invertible");
    let quad_page = [[10.0, 50.0], [160.0, 50.0], [160.0, 90.0], [10.0, 90.0]];
    let mut quad = [[0.0f32; 2]; 4];
    for (i, [x, y]) in quad_page.iter().enumerate() {
        let (px, py) = inv.map(*x, *y);
        quad[i] = [px, py];
    }
    let lines = vec![line("Hola mundo", quad)];
    let source_fonts = input
        .source_fonts(0, &raster, &lines)
        .expect("fuentes de origen");
    assert!(
        source_fonts[0].is_some(),
        "no se asoció el texto del PDF girado"
    );

    let pages = vec![PageContent {
        image: raster.image,
        to_page: raster.to_page,
        width_pt: raster.width_pt,
        height_pt: raster.height_pt,
        rotation: raster.rotation,
        scanned: raster.scanned,
        from_pdf: true,
        source_fonts,
        lines,
    }];
    let plan = build(&pages, &ReplaceOptions::default()).expect("plan");
    assert!(
        plan.pages[0].overlay,
        "una página rotada debe ir en overlay"
    );

    let out = temp_path("visible-vector-rot90.pdf");
    write_visible_pdf(Some(&src), &pages, &plan, EraseMode::Real, &out).expect("escribir");

    let doc = Document::load(&out).expect("abrir la salida");
    let page_id = *doc.get_pages().values().next().unwrap();
    let page = doc.get_dictionary(page_id).expect("página");
    assert_eq!(
        page.get(b"Rotate").and_then(Object::as_i64).unwrap(),
        90,
        "se perdió la rotación de la página"
    );
    let media = page.get(b"MediaBox").and_then(Object::as_array).unwrap();
    let dims: Vec<i64> = media.iter().filter_map(|o| o.as_i64().ok()).collect();
    assert_eq!(dims, vec![0, 0, 200, 100], "se alteró el MediaBox");

    let content = String::from_utf8_lossy(&doc.get_page_content(page_id)).into_owned();
    assert!(
        content.contains("(Hola mundo)"),
        "se perdió el texto original: {content}"
    );
    assert!(content.contains("Tj"), "falta el texto nuevo");
}
