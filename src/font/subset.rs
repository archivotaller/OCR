//! Subconjunto de fuentes TrueType para incrustarlas en el PDF.
//!
//! Se conservan solo las tablas necesarias (`glyf`, `loca`, `head`, `hhea`,
//! `hmtx`, `maxp`, `name`, `post`, `OS/2` y las de hinting) con los glifos
//! realmente usados. Los glifos compuestos se remapean para que sus componentes
//! apunten a los nuevos índices.
//!
//! El resultado se usa como `FontFile2` de un `CIDFontType2` con
//! `CIDToGIDMap /Identity`, de forma que el PDF dibuja texto real y editable.

use super::FontSource;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use ttf_parser::Face;

/// Fuente subconjuntada lista para incrustar en el PDF.
pub struct SubsetFont {
    /// Bytes del TTF resultante.
    pub data: Vec<u8>,
    pub family: String,
    pub subfamily: String,
    /// Unidades por em de la fuente original.
    pub units_per_em: u16,
    pub ascent: i16,
    pub descent: i16,
    pub cap_height: i16,
    pub bbox: [i16; 4],
    pub italic_angle: f32,
    pub flags: u32,
    pub stem_v: u16,
    /// Carácter -> glifo **nuevo** (que también es el CID).
    pub gid_for_char: HashMap<char, u16>,
    /// Ancho de cada glifo nuevo, en unidades de 1/1000 em.
    pub widths: Vec<u16>,
    /// Glifo nuevo -> carácter, para el CMap ToUnicode.
    pub to_unicode: Vec<(u16, char)>,
}

impl SubsetFont {
    /// Nombre PostScript del subconjunto (con el prefijo de 6 letras que exige
    /// la especificación para fuentes subconjuntadas).
    pub fn pdf_name(&self, tag: &str) -> String {
        let mut base: String = self
            .family
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        base.retain(|c| c != '-');
        if base.is_empty() {
            base.push_str("Font");
        }
        format!("{tag}+{base}")
    }
}

// --- Lectura directa del directorio sfnt (necesaria para copiar `glyf`) -----

fn be_u16(data: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([data[at], data[at + 1]])
}

fn be_i16(data: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([data[at], data[at + 1]])
}

fn be_u32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

/// Vista de las tablas de un sfnt, incluido un índice dentro de una colección.
struct TableDir<'a> {
    data: &'a [u8],
    base: usize,
}

impl<'a> TableDir<'a> {
    fn new(data: &'a [u8], index: u32) -> Result<Self> {
        if data.len() < 12 {
            bail!("fuente truncada");
        }
        let base = if &data[0..4] == b"ttcf" {
            let n = be_u32(data, 8) as usize;
            if index as usize >= n {
                bail!("la colección solo tiene {n} fuentes y se pidió el índice {index}");
            }
            be_u32(data, 12 + 4 * index as usize) as usize
        } else {
            0
        };
        if base + 12 > data.len() {
            bail!("directorio de tablas fuera de rango");
        }
        Ok(TableDir { data, base })
    }

    fn table(&self, tag: &[u8; 4]) -> Option<&'a [u8]> {
        let num = be_u16(self.data, self.base + 4) as usize;
        for i in 0..num {
            let rec = self.base + 12 + i * 16;
            if rec + 16 > self.data.len() {
                return None;
            }
            if &self.data[rec..rec + 4] == tag {
                let offset = be_u32(self.data, rec + 8) as usize;
                let length = be_u32(self.data, rec + 12) as usize;
                if offset + length <= self.data.len() {
                    return Some(&self.data[offset..offset + length]);
                }
                return None;
            }
        }
        None
    }
}

/// Glifos componentes de un glifo compuesto (vacío si es simple).
fn component_gids(glyph: &[u8]) -> Vec<u16> {
    let mut out = Vec::new();
    if glyph.len() < 10 || be_i16(glyph, 0) >= 0 {
        return out;
    }
    let mut p = 10usize;
    loop {
        if p + 4 > glyph.len() {
            break;
        }
        let flags = be_u16(glyph, p);
        out.push(be_u16(glyph, p + 2));
        p += 4;
        p += if flags & 0x0001 != 0 { 4 } else { 2 };
        if flags & 0x0008 != 0 {
            p += 2;
        } else if flags & 0x0040 != 0 {
            p += 4;
        } else if flags & 0x0080 != 0 {
            p += 8;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
    out
}

/// Copia un glifo compuesto sustituyendo los índices de sus componentes.
fn patch_composites(glyph: &[u8], map: &HashMap<u16, u16>) -> Vec<u8> {
    let mut out = glyph.to_vec();
    if out.len() < 10 || be_i16(&out, 0) >= 0 {
        return out;
    }
    let mut p = 10usize;
    loop {
        if p + 4 > out.len() {
            break;
        }
        let flags = be_u16(&out, p);
        let old = be_u16(&out, p + 2);
        if let Some(new) = map.get(&old) {
            out[p + 2..p + 4].copy_from_slice(&new.to_be_bytes());
        }
        p += 4;
        p += if flags & 0x0001 != 0 { 4 } else { 2 };
        if flags & 0x0008 != 0 {
            p += 2;
        } else if flags & 0x0040 != 0 {
            p += 4;
        } else if flags & 0x0080 != 0 {
            p += 8;
        }
        if flags & 0x0020 == 0 {
            break;
        }
    }
    out
}

// --- Construcción del sfnt --------------------------------------------------

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < data.len() {
        let mut word = [0u8; 4];
        let n = (data.len() - i).min(4);
        word[..n].copy_from_slice(&data[i..i + n]);
        sum = sum.wrapping_add(u32::from_be_bytes(word));
        i += 4;
    }
    sum
}

/// Ensambla las tablas en un sfnt con los checksums correctos.
fn assemble(mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by(|a, b| a.0.cmp(&b.0));
    let n = tables.len();
    let mut search_range = 1usize;
    let mut entry_selector = 0usize;
    while search_range * 2 <= n {
        search_range *= 2;
        entry_selector += 1;
    }
    search_range *= 16;
    let range_shift = n * 16 - search_range;

    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&(search_range as u16).to_be_bytes());
    out.extend_from_slice(&(entry_selector as u16).to_be_bytes());
    out.extend_from_slice(&(range_shift as u16).to_be_bytes());

    let header_len = 12 + 16 * n;
    let mut offset = header_len;
    let mut records = Vec::with_capacity(n);
    for (tag, data) in &tables {
        let length = data.len();
        let padded = (length + 3) & !3;
        records.push((*tag, checksum(data), offset, length));
        offset += padded;
    }
    for (tag, sum, off, len) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&sum.to_be_bytes());
        out.extend_from_slice(&(*off as u32).to_be_bytes());
        out.extend_from_slice(&(*len as u32).to_be_bytes());
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }

    // Ajuste de checksum global en la tabla `head`.
    if let Some((_, _, head_off, _)) = records.iter().find(|(tag, ..)| tag == b"head") {
        let sum = checksum(&out);
        let adj = 0xB1B0_AFBAu32.wrapping_sub(sum);
        let at = head_off + 8;
        out[at..at + 4].copy_from_slice(&adj.to_be_bytes());
    }
    out
}

fn build_name_table(family: &str, subfamily: &str) -> Vec<u8> {
    let entries: Vec<(u16, String)> = vec![
        (1, family.to_string()),
        (2, subfamily.to_string()),
        (4, format!("{family} {subfamily}")),
        (6, format!("{family}-{subfamily}").replace(' ', "")),
    ];
    let mut strings: Vec<u8> = Vec::new();
    let mut records: Vec<(u16, usize, usize)> = Vec::new();
    for (id, text) in &entries {
        let mut encoded = Vec::new();
        for u in text.encode_utf16() {
            encoded.extend_from_slice(&u.to_be_bytes());
        }
        records.push((*id, strings.len(), encoded.len()));
        strings.extend_from_slice(&encoded);
    }

    let count = records.len() as u16;
    let string_offset = 6 + 12 * records.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // format
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&(string_offset as u16).to_be_bytes());
    for (id, off, len) in records {
        out.extend_from_slice(&3u16.to_be_bytes()); // Windows
        out.extend_from_slice(&1u16.to_be_bytes()); // Unicode BMP
        out.extend_from_slice(&0x0409u16.to_be_bytes()); // en-US
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.extend_from_slice(&(off as u16).to_be_bytes());
    }
    out.extend_from_slice(&strings);
    out
}

fn post_table_v3() -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0x0003_0000u32.to_be_bytes());
    out.extend_from_slice(&0i32.to_be_bytes()); // italicAngle
    out.extend_from_slice(&(-100i16).to_be_bytes()); // underlinePosition
    out.extend_from_slice(&50i16.to_be_bytes()); // underlineThickness
    out.extend_from_slice(&0u32.to_be_bytes()); // isFixedPitch
    out.extend_from_slice(&[0u8; 16]); // memoria reservada
    out
}

/// Heurística para el flag `Serif` del descriptor: solo afecta a la sustitución
/// de fuentes del visor, no al dibujo.
fn looks_serif(family: &str) -> bool {
    let f = family.to_lowercase();
    [
        "serif", "times", "georgia", "garamond", "book", "roman", "宋体", "明朝",
    ]
    .iter()
    .any(|needle| f.contains(needle))
        && !f.contains("sans")
}

/// Crea el subconjunto de `source` con los caracteres de `chars`.
pub fn build_subset(source: &FontSource, chars: &BTreeSet<char>) -> Result<SubsetFont> {
    let face = source.face()?;
    if face.tables().glyf.is_none() {
        bail!(
            "'{}' no tiene contornos TrueType (`glyf`); usa un TTF en lugar de un OTF/CFF",
            source.label
        );
    }
    if face.tables().os2.is_some()
        && (!face.is_subsetting_allowed() || !face.is_outline_embedding_allowed())
    {
        bail!(
            "'{}' no permite incrustar subconjuntos (fsType)",
            source.label
        );
    }

    let dir = TableDir::new(&source.data, source.index)?;
    let head = dir
        .table(b"head")
        .context("la fuente no tiene tabla head")?;
    let maxp = dir
        .table(b"maxp")
        .context("la fuente no tiene tabla maxp")?;
    let hhea = dir
        .table(b"hhea")
        .context("la fuente no tiene tabla hhea")?;
    let loca = dir
        .table(b"loca")
        .context("la fuente no tiene tabla loca")?;
    let glyf = dir
        .table(b"glyf")
        .context("la fuente no tiene tabla glyf")?;

    let num_glyphs = be_u16(maxp, 4) as usize;
    let long_loca = be_i16(head, 50) != 0;
    let offsets: Vec<u32> = (0..=num_glyphs)
        .map(|i| {
            if long_loca {
                if (i * 4 + 4) <= loca.len() {
                    be_u32(loca, i * 4)
                } else {
                    0
                }
            } else if (i * 2 + 2) <= loca.len() {
                be_u16(loca, i * 2) as u32 * 2
            } else {
                0
            }
        })
        .collect();

    let glyph_data = |gid: u16| -> &[u8] {
        let g = gid as usize;
        if g + 1 >= offsets.len() {
            return &[];
        }
        let start = offsets[g] as usize;
        let end = offsets[g + 1] as usize;
        if start >= end || end > glyf.len() {
            return &[];
        }
        &glyf[start..end]
    };

    // 1. Glifos usados: los de los caracteres y, recursivamente, sus componentes.
    let mut used: BTreeSet<u16> = BTreeSet::new();
    used.insert(0); // .notdef
    let mut pending: Vec<u16> = Vec::new();
    for ch in chars {
        if let Some(gid) = face.glyph_index(*ch) {
            if used.insert(gid.0) {
                pending.push(gid.0);
            }
        }
    }
    while let Some(gid) = pending.pop() {
        for component in component_gids(glyph_data(gid)) {
            if used.insert(component) {
                pending.push(component);
            }
        }
    }

    // 2. Asignación de nuevos índices (0 = .notdef) y mapa inverso.
    let old_gids: Vec<u16> = used.into_iter().collect();
    let mut map: HashMap<u16, u16> = HashMap::new();
    for (new, old) in old_gids.iter().enumerate() {
        map.insert(*old, new as u16);
    }

    // 3. `glyf` + `loca` nuevos.
    let mut new_glyf: Vec<u8> = Vec::new();
    let mut new_loca: Vec<u32> = Vec::with_capacity(old_gids.len() + 1);
    for old in &old_gids {
        new_loca.push(new_glyf.len() as u32);
        let original = glyph_data(*old);
        if !original.is_empty() {
            let mut glyph = patch_composites(original, &map);
            new_glyf.append(&mut glyph);
        }
        while !new_glyf.len().is_multiple_of(4) {
            new_glyf.push(0);
        }
    }
    new_loca.push(new_glyf.len() as u32);

    let mut loca_bytes = Vec::with_capacity(new_loca.len() * 4);
    for off in &new_loca {
        loca_bytes.extend_from_slice(&off.to_be_bytes());
    }

    // 4. `hmtx`: anchos y bearings de cada glifo nuevo.
    let number_hmetrics = be_u16(hhea, 34) as usize;
    let hmtx = dir
        .table(b"hmtx")
        .context("la fuente no tiene tabla hmtx")?;
    let last_advance = if number_hmetrics > 0 && number_hmetrics * 4 <= hmtx.len() {
        be_u16(hmtx, (number_hmetrics - 1) * 4)
    } else {
        0
    };
    let mut hmtx_bytes = Vec::with_capacity(old_gids.len() * 4);
    let mut widths = Vec::with_capacity(old_gids.len());
    let upem = face.units_per_em().max(1) as u32;
    for old in &old_gids {
        let (advance, lsb) =
            if (*old as usize) < number_hmetrics && (*old as usize) * 4 + 4 <= hmtx.len() {
                let at = *old as usize * 4;
                (be_u16(hmtx, at), be_i16(hmtx, at + 2))
            } else {
                let index = *old as usize - number_hmetrics;
                let at = number_hmetrics * 4 + index * 2;
                let lsb = if at + 2 <= hmtx.len() {
                    be_i16(hmtx, at)
                } else {
                    0
                };
                (last_advance, lsb)
            };
        let width_1000 = ((advance as u32 * 1000 + upem / 2) / upem) as u16;
        widths.push(width_1000);
        hmtx_bytes.extend_from_slice(&advance.to_be_bytes());
        hmtx_bytes.extend_from_slice(&lsb.to_be_bytes());
    }

    // 5. `head`, `hhea` y `maxp` parcheados.
    let mut head_bytes = head.to_vec();
    if head_bytes.len() < 54 {
        bail!("tabla head inválida");
    }
    head_bytes[8..12].copy_from_slice(&0u32.to_be_bytes()); // checkSumAdjustment
    head_bytes[50..52].copy_from_slice(&1i16.to_be_bytes()); // loca en formato largo

    let mut hhea_bytes = hhea.to_vec();
    if hhea_bytes.len() < 36 {
        bail!("tabla hhea inválida");
    }
    hhea_bytes[34..36].copy_from_slice(&(old_gids.len() as u16).to_be_bytes());

    let mut maxp_bytes = maxp.to_vec();
    if maxp_bytes.len() < 6 {
        bail!("tabla maxp inválida");
    }
    maxp_bytes[4..6].copy_from_slice(&(old_gids.len() as u16).to_be_bytes());

    // 6. Nombres y metadatos del descriptor.
    let family = face
        .names()
        .into_iter()
        .find(|n| n.name_id == 1)
        .and_then(|n| n.to_string())
        .unwrap_or_else(|| source.label.clone());
    let subfamily = face
        .names()
        .into_iter()
        .find(|n| n.name_id == 2)
        .and_then(|n| n.to_string())
        .unwrap_or_else(|| "Regular".into());

    let bbox = face.global_bounding_box();
    let cap_height = face.capital_height().unwrap_or(face.ascender());
    let mut flags: u32 = 32; // Nonsymbolic
    if looks_serif(&family) {
        flags |= 2;
    }
    if face.is_monospaced() {
        flags |= 1;
    }
    if face.is_italic() {
        flags |= 64;
    }
    let stem_v = if face.is_bold() { 120 } else { 85 };

    // 7. ToUnicode y mapa carácter -> glifo nuevo.
    let mut gid_for_char = HashMap::new();
    let mut to_unicode: HashMap<u16, char> = HashMap::new();
    for ch in chars {
        if let Some(gid) = face.glyph_index(*ch) {
            if let Some(new) = map.get(&gid.0) {
                gid_for_char.insert(*ch, *new);
                to_unicode.entry(*new).or_insert(*ch);
            }
        }
    }
    let mut to_unicode: Vec<(u16, char)> = to_unicode.into_iter().collect();
    to_unicode.sort_by_key(|(gid, _)| *gid);

    // 8. Ensamblado final.
    let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"head", head_bytes),
        (*b"hhea", hhea_bytes),
        (*b"maxp", maxp_bytes),
        (*b"hmtx", hmtx_bytes),
        (*b"loca", loca_bytes),
        (*b"glyf", new_glyf),
        (*b"name", build_name_table(&family, &subfamily)),
        (*b"post", post_table_v3()),
    ];
    for tag in [b"OS/2", b"cvt ", b"fpgm", b"prep", b"gasp"] {
        if let Some(data) = dir.table(tag) {
            tables.push((*tag, data.to_vec()));
        }
    }

    Ok(SubsetFont {
        data: assemble(tables),
        family,
        subfamily,
        units_per_em: face.units_per_em(),
        ascent: face.ascender(),
        descent: face.descender(),
        cap_height,
        bbox: [bbox.x_min, bbox.y_min, bbox.x_max, bbox.y_max],
        italic_angle: 0.0,
        flags,
        stem_v,
        gid_for_char,
        widths,
        to_unicode,
    })
}

/// Comprueba que un TTF generado se puede volver a interpretar.
pub fn validate(data: &[u8]) -> Result<u16> {
    let face = Face::parse(data, 0).map_err(|e| anyhow::anyhow!("TTF inválido: {e}"))?;
    if face.tables().glyf.is_none() {
        bail!("el subconjunto no tiene tabla glyf");
    }
    Ok(face.number_of_glyphs())
}

/// Glifos de una fuente original (para tests y diagnósticos).
pub fn glyph_count(source: &FontSource) -> HashSet<u16> {
    source
        .face()
        .map(|f| (0..f.number_of_glyphs()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noto() -> FontSource {
        FontSource {
            data: super::super::BUNDLED_SANS.to_vec(),
            index: 0,
            label: "Noto Sans".into(),
            path: None,
        }
    }

    #[test]
    fn subconjunto_valido_y_pequeno() {
        let chars: BTreeSet<char> = "Hola ñandú 123".chars().collect();
        let subset = build_subset(&noto(), &chars).expect("subconjunto");
        assert!(subset.data.len() < 20_000, "{} bytes", subset.data.len());
        let glyphs = validate(&subset.data).expect("TTF válido");
        assert_eq!(glyphs as usize, subset.widths.len());
        for ch in chars {
            assert!(
                subset.gid_for_char.contains_key(&ch),
                "falta {ch:?} en el subconjunto"
            );
        }
    }

    #[test]
    fn glifos_compuestos_remapados() {
        // 'á' suele ser compuesto en Noto Sans.
        let chars: BTreeSet<char> = "áéíóú".chars().collect();
        let subset = build_subset(&noto(), &chars).expect("subconjunto");
        validate(&subset.data).expect("TTF válido");
        for ch in chars {
            let gid = subset.gid_for_char[&ch] as usize;
            assert!((gid as usize) < subset.widths.len());
            assert!(subset.widths[gid] > 0);
        }
    }

    #[test]
    fn anchos_en_mil_em() {
        let chars: BTreeSet<char> = "MWi".chars().collect();
        let subset = build_subset(&noto(), &chars).expect("subconjunto");
        let w = |c: char| subset.widths[subset.gid_for_char[&c] as usize];
        assert!(w('M') > w('i'), "M debería ser más ancha que i");
        assert!((200..=1000).contains(&w('i')));
    }
}
