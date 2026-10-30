//! Generación de PDF buscable: imagen original a página completa + capa de
//! texto invisible (render mode 3) alineada con las cajas del OCR.
//!
//! La capa de texto usa una fuente Type0 con Identity-H y un CMap ToUnicode
//! generado a partir de los caracteres presentes, de modo que cualquier texto
//! (incl. CJK del diccionario) siga siendo copiable y buscable. Los strings se
//! emiten en hex UTF-16BE.

use crate::ocr::engine::OcrLine;
use anyhow::{Context, Result};
use lopdf::content::{Content, Operation};
use lopdf::xobject;
use lopdf::{dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;

/// Detecta si los bytes crudos son un JPEG (para reusarlos sin re-codificar).
fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF
}

/// Codifica `img` como JPEG de calidad alta en memoria.
fn img_to_jpeg(img: &image::DynamicImage) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Cursor::new(Vec::new());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92);
    enc.encode_image(&rgb)
        .with_context(|| "codificando imagen a JPEG")?;
    Ok(buf.into_inner())
}

/// Bytes UTF-16BE de un string (para los operadores de texto PDF).
fn utf16be(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

/// Construye el contenido de un CMap ToUnicode: mapea cada código Identity-H
/// (codepoint UTF-16) a sí mismo, agrupando en rangos consecutivos.
fn tounicode_cmap(codes: &[u16]) -> String {
    let mut s = String::new();
    s.push_str("/CIDInit /ProcSet findresource begin\n");
    s.push_str("12 dict begin\n");
    s.push_str("begincmap\n");
    s.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    s.push_str("/CMapName /Adobe-Identity-UCS def\n");
    s.push_str("/CMapType 2 def\n");
    s.push_str("1 begincodespacerange\n");
    s.push_str("<0000> <FFFF>\n");
    s.push_str("endcodespacerange\n");

    if codes.is_empty() {
        s.push_str("0 beginbfrange\n");
        s.push_str("endbfrange\n");
    } else {
        // Agrupa códigos consecutivos en rangos [start..end] -> [dst..].
        let mut groups: Vec<(u16, u16)> = Vec::new();
        for &c in codes {
            match groups.last_mut() {
                Some((_, end)) if *end + 1 == c => *end = c,
                _ => groups.push((c, c)),
            }
        }
        s.push_str(&format!("{} beginbfrange\n", groups.len()));
        for (start, end) in groups {
            s.push_str(&format!(
                "<{:04X}> <{:04X}> <{:04X}>\n",
                start, end, start
            ));
        }
        s.push_str("endbfrange\n");
    }

    s.push_str("endcmap\n");
    s.push_str("CMapName currentdict /CMap defineresource pop\n");
    s.push_str("end\n");
    s.push_str("end\n");
    s
}

/// Caja (min rect) de una línea en coordenadas de imagen (y hacia abajo).
/// Devuelve (x_izquierda, y_superior_px, ancho, alto) en píxeles.
fn box_bounds(line: &OcrLine) -> (f32, f32, f32, f32) {
    let xs: Vec<f32> = line.points.iter().map(|p| p.0).collect();
    let ys: Vec<f32> = line.points.iter().map(|p| p.1).collect();
    let left = *xs.iter().fold(&f32::MAX, |a, b| if b < a { b } else { a }) as f32;
    let top = *ys.iter().fold(&f32::MAX, |a, b| if b < a { b } else { a }) as f32;
    let right = *xs.iter().fold(&f32::MIN, |a, b| if b > a { b } else { a }) as f32;
    let bottom = *ys.iter().fold(&f32::MIN, |a, b| if b > a { b } else { a }) as f32;
    (left, top, (right - left).max(1.0), (bottom - top).max(1.0))
}

/// Genera el objeto de la fuente Type0 (Identity-H + ToUnicode) y sus
/// dependencias (CIDFont, descriptor, CMap). Devuelve el ObjectId de la Type0.
fn add_unicode_font(doc: &mut Document, chars: &BTreeSet<u16>) -> Result<ObjectId> {
    let base = "ArialMT";
    let cmap_content = tounicode_cmap(
        &chars.iter().copied().collect::<Vec<u16>>(),
    );
    let cmap_id = doc.add_object(Stream::new(dictionary! {}, cmap_content.into_bytes()));

    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => base,
        "Flags" => 32,
        "FontBBox" => vec![Object::Integer(-1000), Object::Integer(-400), Object::Integer(3000), Object::Integer(1200)],
        "ItalicAngle" => 0,
        "Ascent" => 1000,
        "Descent" => -300,
        "CapHeight" => 800,
        "StemV" => 80,
    });

    let cid_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => base,
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("UCS"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => Object::Array(vec![
            Object::Integer(0),
            Object::Array(vec![Object::Integer(1000)]),
        ]),
    });

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => base,
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_id)],
        "ToUnicode" => cmap_id,
    });
    Ok(font_id)
}

/// Construye un PDF con capa de texto invisible y lo guarda en `out`.
///
/// `orig_bytes` son los bytes crudos del archivo origen (si es JPEG se
/// reutilizan byte a byte para no perder calidad); si no, se codifica desde
/// `img`. `img` es la primera página decodificada.
pub fn searchable_pdf(
    orig_bytes: Option<&[u8]>,
    img: &image::DynamicImage,
    lines: &[OcrLine],
    out: &Path,
) -> Result<()> {
    let (w, h) = (img.width() as u32, img.height() as u32);
    if w == 0 || h == 0 {
        anyhow::bail!("imagen vacía para PDF");
    }

    // Imagen: JPEG original o re-codificación a JPEG desde la página decodificada.
    let bytes: Vec<u8> = match orig_bytes {
        Some(b) if is_jpeg(b) => b.to_vec(),
        _ => img_to_jpeg(img)?,
    };
    let image_stream = xobject::image_from(bytes).with_context(|| "incrustando imagen")?;

    let mut doc = Document::with_version("1.7");
    let image_id = doc.add_object(image_stream);
    let img_name = "Im1";

    // Todos los codepoints usados por el texto (para el CMap ToUnicode).
    let mut chars: BTreeSet<u16> = BTreeSet::new();
    for line in lines {
        for u in line.text.encode_utf16() {
            chars.insert(u);
        }
    }
    let font_id = add_unicode_font(&mut doc, &chars)?;
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
        "XObject" => dictionary! {
            img_name => image_id,
        },
    });

    // Flujo de contenido: imagen a página completa + capa invisible.
    let mut ops: Vec<Operation> = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                Object::Real(w as f32),
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(h as f32),
                Object::Real(0.0),
                Object::Real(0.0),
            ],
        ),
        Operation::new("Do", vec![Object::Name(img_name.as_bytes().to_vec())]),
        Operation::new("Q", vec![]),
    ];

    for line in lines {
        if line.text.is_empty() {
            continue;
        }
        let (left, top, box_w, box_h) = box_bounds(line);
        let nchars = line.text.chars().count().max(1) as f32;
        // Tamaño de fuente para que el texto (aprox. 0.55 em por carácter)
        // quepa en la caja, limitado por la altura de la línea.
        let fs = (box_h.min(box_w / (nchars * 0.55))).clamp(4.0, box_h);
        // En PDF y crece hacia arriba: la base del texto queda ~0.15 em bajo
        // el borde superior de la caja.
        let x = left;
        let baseline = (h as f32 - (top + fs * 0.85)).max(0.0);
        ops.extend(vec![
            Operation::new("BT", vec![]),
            Operation::new("Tr", vec![Object::Integer(3)]),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Real(fs)]),
            Operation::new("Td", vec![Object::Real(x), Object::Real(baseline)]),
            Operation::new(
                "Tj",
                vec![Object::String(utf16be(&line.text), StringFormat::Hexadecimal)],
            ),
            Operation::new("ET", vec![]),
        ]);
    }

    let content = Content { operations: ops };
    let content_bytes = content.encode().with_context(|| "codificando contenido")?;
    let content_id = doc.add_object(Stream::new(dictionary! {}, content_bytes));

    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(w as i64), Object::Integer(h as i64)],
    });

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
    doc.compress();
    doc.save(out).with_context(|| format!("guardando {}", out.display()))?;
    Ok(())
}