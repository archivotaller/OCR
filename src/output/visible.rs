//! Segunda salida PDF: borra el texto original en las cajas del OCR y escribe
//! encima el texto reconocido, visible, buscable, copiable y editable.
//!
//! - En páginas **vectoriales** de un PDF se conserva el contenido original y
//!   se superpone el parche (nunca se aplana ni se pierden vectores).
//! - En imágenes/TIFF y páginas escaneadas se reconstruye la página con el
//!   raster (con borrado real de píxeles o capa visual, según `EraseMode`).
//! - El texto usa `Type0`/`Identity-H` + `CIDFontType2` con subconjuntos
//!   incrustados, de modo que sigue siendo editable en un visor PDF.

use crate::font::subset::SubsetFont;
use crate::geom::quad_bounds;
use crate::replace::{erase_pixels, EraseMode, PageContent, PagePlan, Plan};
use anyhow::{anyhow, Context, Result};
use image::ImageEncoder;
use lopdf::content::{Content, Operation};
use lopdf::xobject;
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId, Stream};
use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

/// Nombre de recurso base para las fuentes incrustadas.
const FONT_PREFIX: &str = "OCTXT";

/// Escribe el PDF con el texto reemplazado.
///
/// `original` es el PDF de entrada (si lo hay); `pages` y `plan` describen las
/// páginas ya procesadas.
pub fn write_visible_pdf(
    original: Option<&Path>,
    pages: &[PageContent],
    plan: &Plan,
    mode: EraseMode,
    out: &Path,
) -> Result<()> {
    anyhow::ensure!(!plan.pages.is_empty(), "no hay páginas que escribir");

    let mut doc = match original {
        Some(path) => {
            Document::load(path).with_context(|| format!("abriendo el PDF {}", path.display()))?
        }
        None => Document::with_version("1.7"),
    };

    // 1. Incrustar los subconjuntos usados.
    let font_ids = add_fonts(&mut doc, plan)?;

    let existing_pages: Vec<ObjectId> = if original.is_some() {
        doc.get_pages().into_values().collect()
    } else {
        Vec::new()
    };

    let pages_id = doc.new_object_id();
    let mut kids: Vec<Object> = Vec::with_capacity(plan.pages.len());

    for page_plan in &plan.pages {
        let idx = page_plan.page;
        let page = pages
            .get(idx)
            .ok_or_else(|| anyhow!("el plan referencia la página {idx} pero no existe"))?;

        let old_id = existing_pages.get(idx).copied();
        let names = pick_font_names(
            &doc,
            old_id,
            plan.subsets
                .iter()
                .map(|s| s.is_some())
                .collect::<Vec<_>>()
                .as_slice(),
        )?;

        if page_plan.overlay {
            let Some(page_id) = old_id else {
                return Err(anyhow!(
                    "la página {} se marcó como overlay pero no viene de un PDF",
                    idx + 1
                ));
            };
            let mut content = doc.get_page_content(page_id);
            content.push(b'\n');
            content.extend_from_slice(&build_overlay(page_plan, mode, &names));
            let stream_id = doc.add_object(Stream::new(dictionary! {}, content));
            doc.get_dictionary_mut(page_id)?
                .set("Contents", Object::Reference(stream_id));
            merge_fonts(&mut doc, page_id, &font_ids, &names)?;
            kids.push(Object::Reference(page_id));
            continue;
        }

        // Reconstrucción desde el raster (imagen suelta o página escaneada).
        let (content_id, resources) =
            build_raster_page(&mut doc, page, page_plan, mode, &names, &font_ids)?;

        if let Some(page_id) = old_id {
            // Página escaneada de un PDF: se sustituye el contenido.
            {
                let page_dict = doc.get_dictionary_mut(page_id)?;
                page_dict.set("Contents", Object::Reference(content_id));
                page_dict.set("Resources", Object::Dictionary(resources));
            }
            kids.push(Object::Reference(page_id));
        } else {
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![
                    Object::Real(0.0),
                    Object::Real(0.0),
                    Object::Real(page.width_pt),
                    Object::Real(page.height_pt),
                ],
                "Contents" => content_id,
                "Resources" => Object::Dictionary(resources),
            });
            kids.push(Object::Reference(page_id));
        }
    }

    if original.is_none() {
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids,
                "Count" => plan.pages.len() as i64,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
    }

    doc.compress();
    doc.save(out)
        .with_context(|| format!("guardando {}", out.display()))?;
    Ok(())
}

// --- Fuentes ---------------------------------------------------------------

/// Etiqueta de subconjunto de 6 letras derivada de los datos de la fuente.
fn subset_tag(data: &[u8]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for b in data.iter().step_by(7) {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    (0..6)
        .map(|i| alphabet[((hash >> (i * 5)) & 0x1f) as usize % 26] as char)
        .collect()
}

fn utf16be(ch: char) -> String {
    let mut buf = [0u16; 2];
    ch.encode_utf16(&mut buf)
        .iter()
        .map(|u| format!("{:04X}", u))
        .collect()
}

/// CMap ToUnicode: CID (glifo) -> carácter Unicode.
fn tounicode_cmap(subset: &SubsetFont) -> String {
    let mut s = String::new();
    s.push_str("/CIDInit /ProcSet findresource begin\n");
    s.push_str("12 dict begin\nbegincmap\n");
    s.push_str("/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n");
    s.push_str("/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n");
    s.push_str("1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
    let entries: Vec<String> = subset
        .to_unicode
        .iter()
        .map(|(cid, ch)| format!("<{:04X}> <{}>", cid, utf16be(*ch)))
        .collect();
    if entries.is_empty() {
        s.push_str("0 beginbfchar\nendbfchar\n");
    } else {
        s.push_str(&format!("{} beginbfchar\n", entries.len()));
        for e in entries {
            s.push_str(&e);
            s.push('\n');
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s
}

/// Crea los objetos de una fuente Type0 a partir del subconjunto.
fn add_font(doc: &mut Document, subset: &SubsetFont) -> Result<ObjectId> {
    let tag = subset_tag(&subset.data);
    let base = subset.pdf_name(&tag);
    let upem = subset.units_per_em.max(1) as f32;
    let scale = |v: i16| (v as f32 * 1000.0 / upem).round() as i64;

    let cmap_id = doc.add_object(Stream::new(
        dictionary! {},
        tounicode_cmap(subset).into_bytes(),
    ));

    let font_file_id = doc.add_object(Stream::new(
        dictionary! {
            "Length1" => subset.data.len() as i64,
        },
        subset.data.clone(),
    ));

    let descriptor_id = doc.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => base.clone(),
        "Flags" => subset.flags as i64,
        "FontBBox" => vec![
            Object::Integer(scale(subset.bbox[0])),
            Object::Integer(scale(subset.bbox[1])),
            Object::Integer(scale(subset.bbox[2])),
            Object::Integer(scale(subset.bbox[3])),
        ],
        "ItalicAngle" => subset.italic_angle,
        "Ascent" => scale(subset.ascent),
        "Descent" => scale(subset.descent),
        "CapHeight" => scale(subset.cap_height),
        "StemV" => subset.stem_v as i64,
        "FontFile2" => font_file_id,
    });

    let widths: Vec<Object> = subset
        .widths
        .iter()
        .map(|w| Object::Integer(*w as i64))
        .collect();
    let cid_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => base.clone(),
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "CIDToGIDMap" => "Identity",
        "DW" => 1000,
        "W" => Object::Array(vec![
            Object::Integer(0),
            Object::Array(widths),
        ]),
    });

    Ok(doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => base,
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_id)],
        "ToUnicode" => cmap_id,
    }))
}

fn add_fonts(doc: &mut Document, plan: &Plan) -> Result<Vec<Option<ObjectId>>> {
    let mut out = Vec::with_capacity(plan.subsets.len());
    for subset in &plan.subsets {
        match subset {
            Some(s) => out.push(Some(add_font(doc, s)?)),
            None => out.push(None),
        }
    }
    Ok(out)
}

/// Nombres de recurso únicos para las fuentes de esta página (sin chocar con
/// las que ya usa el PDF original).
fn pick_font_names(
    doc: &Document,
    page_id: Option<ObjectId>,
    used: &[bool],
) -> Result<Vec<String>> {
    let mut taken: HashSet<Vec<u8>> = HashSet::new();
    if let Some(page_id) = page_id {
        taken = existing_font_names(doc, page_id)?;
    }
    let mut names = Vec::with_capacity(used.len());
    for (i, present) in used.iter().enumerate() {
        if !present {
            names.push(String::new());
            continue;
        }
        let mut name = format!("{FONT_PREFIX}{i}");
        let mut n = 0;
        while taken.contains(name.as_bytes()) {
            n += 1;
            name = format!("{FONT_PREFIX}{i}_{n}");
        }
        taken.insert(name.as_bytes().to_vec());
        names.push(name);
    }
    Ok(names)
}

fn existing_font_names(doc: &Document, page_id: ObjectId) -> Result<HashSet<Vec<u8>>> {
    let resources = effective_resources(doc, page_id)?;
    let mut out = HashSet::new();
    if let Ok(fonts) = resolved_subdict(doc, &resources, b"Font") {
        for (k, _) in fonts.iter() {
            out.insert(k.clone());
        }
    }
    Ok(out)
}

fn effective_resources(doc: &Document, page_id: ObjectId) -> Result<Dictionary> {
    let page = doc.get_dictionary(page_id)?;
    if let Ok(d) = page.get(b"Resources").and_then(Object::as_dict) {
        return Ok(d.clone());
    }
    if let Ok(id) = page.get(b"Resources").and_then(Object::as_reference) {
        return Ok(doc.get_dictionary(id)?.clone());
    }
    // Recursos heredados del árbol de páginas.
    let mut parent = page.get(b"Parent").and_then(Object::as_reference).ok();
    for _ in 0..64 {
        let Some(pid) = parent else { break };
        let p = doc.get_dictionary(pid)?;
        if let Ok(d) = p.get(b"Resources").and_then(Object::as_dict) {
            return Ok(d.clone());
        }
        if let Ok(id) = p.get(b"Resources").and_then(Object::as_reference) {
            return Ok(doc.get_dictionary(id)?.clone());
        }
        parent = p.get(b"Parent").and_then(Object::as_reference).ok();
    }
    Ok(Dictionary::new())
}

fn resolved_subdict(doc: &Document, parent: &Dictionary, key: &[u8]) -> Result<Dictionary> {
    match parent.get(key) {
        Ok(Object::Dictionary(d)) => Ok(d.clone()),
        Ok(Object::Reference(id)) => Ok(doc.get_dictionary(*id)?.clone()),
        _ => Ok(Dictionary::new()),
    }
}

fn merge_into_subdict(
    doc: &Document,
    resources: &mut Dictionary,
    key: &str,
    entries: &[(Vec<u8>, ObjectId)],
) -> Result<()> {
    let mut sub = resolved_subdict(doc, resources, key.as_bytes())?;
    for (name, id) in entries {
        sub.set(name.clone(), Object::Reference(*id));
    }
    resources.set(key, Object::Dictionary(sub));
    Ok(())
}

fn merge_fonts(
    doc: &mut Document,
    page_id: ObjectId,
    font_ids: &[Option<ObjectId>],
    names: &[String],
) -> Result<()> {
    let mut resources = effective_resources(doc, page_id)?;
    let entries: Vec<(Vec<u8>, ObjectId)> = font_ids
        .iter()
        .zip(names)
        .filter_map(|(id, name)| id.map(|id| (name.as_bytes().to_vec(), id)))
        .collect();
    merge_into_subdict(doc, &mut resources, "Font", &entries)?;
    doc.get_dictionary_mut(page_id)?
        .set("Resources", Object::Dictionary(resources));
    Ok(())
}

// --- Contenido -------------------------------------------------------------

/// Número con precisión fija y sin notación científica.
fn num(v: f32) -> String {
    format!("{v:.3}")
}

fn pdf_color(c: crate::replace::Rgb) -> String {
    format!(
        "{} {} {}",
        num(c.r as f32 / 255.0),
        num(c.g as f32 / 255.0),
        num(c.b as f32 / 255.0)
    )
}

/// Operadores de borrado + texto visible de una página.
fn build_overlay(page: &PagePlan, mode: EraseMode, font_names: &[String]) -> Vec<u8> {
    let mut s = String::new();
    let draw_rects = page.overlay || mode == EraseMode::Visual;

    if draw_rects {
        for line in &page.lines {
            s.push_str("q\n");
            s.push_str(&pdf_color(line.background));
            s.push_str(" rg\n");
            for (i, p) in line.erase_pt.iter().enumerate() {
                s.push_str(&format!(
                    "{} {} {}\n",
                    num(p[0]),
                    num(p[1]),
                    if i == 0 { "m" } else { "l" }
                ));
            }
            s.push_str("h f Q\n");
        }
    }

    for line in &page.lines {
        if line.runs.is_empty() {
            continue;
        }
        s.push_str("BT\n");
        s.push_str(&pdf_color(line.text_color));
        s.push_str(" rg\n");
        if line.bold {
            s.push_str(&pdf_color(line.text_color));
            s.push_str(" RG\n");
            s.push_str(&format!("{} w\n", num((line.size_pt * 0.03).max(0.2))));
            s.push_str("2 Tr\n");
        } else {
            s.push_str("0 Tr\n");
        }
        for run in &line.runs {
            let Some(name) = font_names.get(run.font).filter(|n| !n.is_empty()) else {
                continue;
            };
            s.push_str(&format!("/{} {} Tf\n", name, num(line.size_pt)));
            let skew = if line.italic { 0.2126 } else { 0.0 };
            s.push_str(&format!(
                "1 0 {} 1 {} {} Tm\n",
                num(skew),
                num(run.x),
                num(line.baseline.1)
            ));
            s.push('<');
            for cid in &run.cids {
                s.push_str(&format!("{cid:04X}"));
            }
            s.push_str("> Tj\n");
        }
        s.push_str("ET\n");
    }

    s.into_bytes()
}

/// Crea la imagen de la página (con borrado real si procede) y devuelve el
/// stream de contenido y los recursos.
fn build_raster_page(
    doc: &mut Document,
    page: &PageContent,
    page_plan: &PagePlan,
    mode: EraseMode,
    names: &[String],
    font_ids: &[Option<ObjectId>],
) -> Result<(ObjectId, Dictionary)> {
    let mut rgb = page.image.to_rgb8();
    let changed = mode == EraseMode::Real && !page_plan.lines.is_empty();
    if changed {
        erase_pixels(page_plan, mode, &mut rgb);
    }

    let bytes = encode_raster(&rgb)?;
    let image_stream = xobject::image_from(bytes).with_context(|| "incrustando la imagen")?;
    let image_id = doc.add_object(image_stream);

    // La imagen ocupa exactamente el rectángulo que le corresponde en la
    // página, según la transformación del raster.
    let (w, h) = (rgb.width() as f32, rgb.height() as f32);
    let corners = [
        page.to_page.map(0.0, 0.0),
        page.to_page.map(w, 0.0),
        page.to_page.map(w, h),
        page.to_page.map(0.0, h),
    ];
    let quad = [
        [corners[0].0, corners[0].1],
        [corners[1].0, corners[1].1],
        [corners[2].0, corners[2].1],
        [corners[3].0, corners[3].1],
    ];
    let (x0, y0, x1, y1) = quad_bounds(&quad);
    let (dw, dh) = ((x1 - x0).max(0.01), (y1 - y0).max(0.01));

    let mut ops: Vec<Operation> = vec![
        Operation::new("q", vec![]),
        Operation::new(
            "cm",
            vec![
                Object::Real(dw),
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(dh),
                Object::Real(x0),
                Object::Real(y0),
            ],
        ),
        Operation::new("Do", vec![Object::Name(b"Im0".to_vec())]),
        Operation::new("Q", vec![]),
    ];
    let mut content = Content {
        operations: ops.drain(..),
    }
    .encode()?;
    content.push(b'\n');
    content.extend_from_slice(&build_overlay(page_plan, mode, names));

    let content_id = doc.add_object(Stream::new(dictionary! {}, content));

    let mut resources = Dictionary::new();
    let mut font_dict = Dictionary::new();
    for (id, name) in font_ids.iter().zip(names) {
        if let (Some(id), false) = (id, name.is_empty()) {
            font_dict.set(name.as_bytes().to_vec(), Object::Reference(*id));
        }
    }
    resources.set("Font", Object::Dictionary(font_dict));
    resources.set(
        "XObject",
        Object::Dictionary(dictionary! {
            "Im0" => image_id,
        }),
    );
    Ok((content_id, resources))
}

/// Codifica el raster de la página sin pérdida: PNG.
///
/// La página reconstruida sustituye al original, así que no se puede degradar
/// con una codificación con pérdida (JPEG); el PNG se decodifica a píxeles y
/// lopdf lo comprime con Flate en el PDF final.
fn encode_raster(img: &image::RgbImage) -> Result<Vec<u8>> {
    let mut buf = Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgb8,
        )
        .with_context(|| "codificando la página a PNG")?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontSource;
    use crate::replace::Rgb;
    use std::collections::BTreeSet;

    #[test]
    fn cmap_incluye_todos_los_glifos() {
        let source = FontSource {
            data: crate::font::BUNDLED_SANS.to_vec(),
            index: 0,
            label: "Noto Sans".into(),
            path: None,
        };
        let chars: BTreeSet<char> = "Hola ñ".chars().collect();
        let subset = crate::font::subset::build_subset(&source, &chars).unwrap();
        let cmap = tounicode_cmap(&subset);
        assert!(cmap.contains("begincmap"));
        assert!(cmap.contains("beginbfchar"));
        assert!(cmap.contains("<0048>")); // 'H'
        assert_eq!(subset.to_unicode.len(), chars.len());
    }

    #[test]
    fn overlay_dibuja_rect_y_texto() {
        let page = PagePlan {
            page: 0,
            overlay: true,
            non_uniform: false,
            lines: vec![crate::replace::LinePlan {
                line: 0,
                text: "ab".into(),
                erase_px: [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
                erase_pt: [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
                background: Rgb::WHITE,
                uniform_background: true,
                text_color: Rgb::BLACK,
                size_pt: 12.0,
                baseline: (1.0, 2.0),
                runs: vec![crate::replace::TextRun {
                    font: 0,
                    x: 1.0,
                    cids: vec![3, 4],
                    advance: 14.0,
                }],
                bold: false,
                italic: false,
                source: None,
            }],
            skipped: Vec::new(),
        };
        let data = build_overlay(&page, EraseMode::Real, &["F0".into()]);
        let s = String::from_utf8(data).unwrap();
        assert!(s.contains("1.000 1.000 1.000 rg"), "{s}");
        assert!(s.contains("BT"), "{s}");
        assert!(s.contains("<00030004> Tj"), "{s}");
        assert!(s.contains("/F0 12.000 Tf"), "{s}");
    }
}
