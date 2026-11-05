//! Fuentes para el texto visible del PDF: fuente incluida para latín, fuentes
//! del sistema para CJK y utilidades para elegir/validar la fuente de origen.
//!
//! El objetivo es incrustar un **subconjunto** real de cada fuente en el PDF,
//! de modo que el texto sea editable además de visible, buscable y copiable.

pub mod subset;

use crate::font::subset::{build_subset, SubsetFont};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use ttf_parser::Face;

/// Fuente incluida por defecto para latín (Noto Sans, OFL-1.1).
pub const BUNDLED_SANS: &[u8] = include_bytes!("../../assets/fonts/NotoSans-Regular.ttf");
/// Fuente incluida para documentos con serifa (Noto Serif, OFL-1.1).
pub const BUNDLED_SERIF: &[u8] = include_bytes!("../../assets/fonts/NotoSerif-Regular.ttf");

/// Una fuente cargada en memoria (archivo TTF/OTF o colección TTC).
#[derive(Clone)]
pub struct FontSource {
    pub data: Vec<u8>,
    pub index: u32,
    pub label: String,
    pub path: Option<PathBuf>,
}

impl std::fmt::Debug for FontSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSource")
            .field("label", &self.label)
            .field("index", &self.index)
            .field("path", &self.path)
            .finish()
    }
}

impl FontSource {
    /// Carga una fuente desde disco. En colecciones TTC se elige `index`.
    pub fn from_path(path: &Path, index: u32) -> Result<Self> {
        let data =
            std::fs::read(path).with_context(|| format!("leyendo la fuente {}", path.display()))?;
        let face = Face::parse(&data, index)
            .map_err(|e| anyhow!("'{}' no es una fuente TrueType válida: {e}", path.display()))?;
        let label = family_name(&face).unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "fuente".into())
        });
        Ok(FontSource {
            data,
            index,
            label,
            path: Some(path.to_path_buf()),
        })
    }

    fn from_bytes(data: &[u8], label: &str) -> Self {
        FontSource {
            data: data.to_vec(),
            index: 0,
            label: label.to_string(),
            path: None,
        }
    }

    pub fn face(&self) -> Result<Face<'_>> {
        Face::parse(&self.data, self.index)
            .map_err(|e| anyhow!("no se pudo interpretar la fuente '{}': {e}", self.label))
    }

    /// `true` si la fuente tiene un glifo para `ch`.
    pub fn has_glyph(&self, ch: char) -> bool {
        self.face()
            .map(|f| f.glyph_index(ch).is_some())
            .unwrap_or(false)
    }

    /// Comprueba que la licencia de la fuente permite incrustar contornos.
    fn check_embeddable(&self) -> Result<()> {
        let face = self.face()?;
        if face.tables().glyf.is_none() {
            bail!(
                "'{}' usa contornos CFF/OTF, que aún no se pueden incrustar; \
                 prueba con un TTF (por ejemplo Noto Sans o Arial)",
                self.label
            );
        }
        if face.tables().os2.is_some()
            && (!face.is_subsetting_allowed() || !face.is_outline_embedding_allowed())
        {
            bail!(
                "la licencia de '{}' no permite incrustar un subconjunto editable (fsType)",
                self.label
            );
        }
        Ok(())
    }
}

/// Elección de fuentes para el documento.
#[derive(Debug, Clone, Default)]
pub struct FontSpec {
    /// Fuente principal para latín (`--font`). Si falta, Noto Sans/Serif incluida.
    pub latin: Option<PathBuf>,
    /// Índice dentro de una colección TTC para `latin` / `cjk`.
    pub latin_index: u32,
    pub cjk_index: u32,
    /// Usar la fuente incluida con serifa en lugar de la sans.
    pub serif: bool,
    /// Fuente CJK explícita (`--font-cjk`). Si falta, se busca en el sistema.
    pub cjk: Option<PathBuf>,
    /// Fuentes adicionales de reserva, en orden.
    pub extra: Vec<PathBuf>,
}

/// Conjunto de fuentes resuelto: principal + reservas (CJK, extras).
pub struct FontSet {
    sources: Vec<FontSource>,
    cache: HashMap<char, Option<(usize, u16)>>,
}

impl FontSet {
    /// Resuelve la configuración a fuentes reales y valida que se puedan
    /// incrustar. La primera es la principal; el resto son reservas.
    pub fn load(spec: &FontSpec) -> Result<Self> {
        let mut sources: Vec<FontSource> = Vec::new();

        match &spec.latin {
            Some(path) => sources.push(FontSource::from_path(path, spec.latin_index)?),
            None if spec.serif => {
                sources.push(FontSource::from_bytes(BUNDLED_SERIF, "Noto Serif"));
            }
            None => sources.push(FontSource::from_bytes(BUNDLED_SANS, "Noto Sans")),
        }

        if let Some(path) = &spec.cjk {
            sources.push(FontSource::from_path(path, spec.cjk_index)?);
        } else if let Some(cjk) = discover_cjk() {
            sources.push(cjk);
        }

        for path in &spec.extra {
            sources.push(FontSource::from_path(path, 0)?);
        }

        for source in &sources {
            source.check_embeddable()?;
        }

        Ok(FontSet {
            sources,
            cache: HashMap::new(),
        })
    }

    /// Fuente principal (la que se usa para latín).
    pub fn primary(&self) -> &FontSource {
        &self.sources[0]
    }

    /// Todas las fuentes, en orden de preferencia.
    pub fn sources(&self) -> &[FontSource] {
        &self.sources
    }

    /// Resuelve un carácter a `(índice de fuente, glifo)` probando en orden.
    pub fn resolve(&mut self, ch: char) -> Option<(usize, u16)> {
        if let Some(hit) = self.cache.get(&ch) {
            return *hit;
        }
        let mut found = None;
        for (i, source) in self.sources.iter().enumerate() {
            if let Ok(face) = source.face() {
                if let Some(gid) = face.glyph_index(ch) {
                    found = Some((i, gid.0));
                    break;
                }
            }
        }
        self.cache.insert(ch, found);
        found
    }

    /// `true` si todos los caracteres de `text` tienen glifo.
    pub fn can_render(&mut self, text: &str) -> bool {
        text.chars().all(|c| c == ' ' || self.resolve(c).is_some())
    }

    /// Caracteres de `text` que ninguna fuente puede dibujar.
    pub fn missing_chars(&mut self, text: &str) -> Vec<char> {
        let mut out: Vec<char> = Vec::new();
        for c in text.chars() {
            if c == ' ' || self.resolve(c).is_some() {
                continue;
            }
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// Construye el subconjunto de cada fuente usada por `texts`.
    ///
    /// Devuelve, por índice de fuente, el subconjunto ya listo para incrustar.
    pub fn build_subsets(&mut self, texts: &[&str]) -> Result<Vec<Option<SubsetFont>>> {
        let mut per_font: Vec<BTreeSet<char>> = vec![BTreeSet::new(); self.sources.len()];
        for text in texts {
            for ch in text.chars() {
                if let Some((i, _)) = self.resolve(ch) {
                    per_font[i].insert(ch);
                }
            }
        }
        let mut out: Vec<Option<SubsetFont>> = Vec::with_capacity(self.sources.len());
        for (i, chars) in per_font.iter().enumerate() {
            if chars.is_empty() {
                out.push(None);
                continue;
            }
            let subset = build_subset(&self.sources[i], chars).with_context(|| {
                format!("creando el subconjunto de '{}'", self.sources[i].label)
            })?;
            out.push(Some(subset));
        }
        Ok(out)
    }
}

/// Nombre de familia legible de una fuente.
fn family_name(face: &Face<'_>) -> Option<String> {
    for name in face.names() {
        if name.name_id == 1 {
            if let Some(s) = name.to_string() {
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

/// Busca una fuente CJK utilizable en el sistema.
pub fn discover_cjk() -> Option<FontSource> {
    for (path, index) in cjk_candidates() {
        if !path.exists() {
            continue;
        }
        let Ok(source) = FontSource::from_path(&path, index) else {
            continue;
        };
        // Debe tener glifos CJK y permitir incrustación.
        if source.has_glyph('中') && source.check_embeddable().is_ok() {
            return Some(source);
        }
    }
    None
}

fn cjk_candidates() -> Vec<(PathBuf, u32)> {
    let mut out: Vec<(PathBuf, u32)> = Vec::new();

    #[cfg(windows)]
    {
        let dir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:/Windows"))
            .join("Fonts");
        for name in [
            "msyh.ttc",
            "msyhbd.ttc",
            "simsun.ttc",
            "msgothic.ttc",
            "malgun.ttf",
        ] {
            out.push((dir.join(name), 0));
        }
    }

    #[cfg(target_os = "macos")]
    {
        for path in [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ] {
            out.push((PathBuf::from(path), 0));
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        for path in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        ] {
            out.push((PathBuf::from(path), 0));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_fuente_incluida_cubre_latino() {
        let mut set = FontSet::load(&FontSpec::default()).unwrap();
        for ch in "Hola, ¿qué tal? — ñandú".chars() {
            if ch == ' ' {
                continue;
            }
            assert!(set.resolve(ch).is_some(), "falta el glifo de {ch:?}");
        }
    }

    #[test]
    fn el_subconjunto_reduce_el_tamano() {
        let mut set = FontSet::load(&FontSpec::default()).unwrap();
        let subsets = set.build_subsets(&["Hola mundo"]).unwrap();
        let subset = subsets[0]
            .as_ref()
            .expect("subconjunto de la fuente principal");
        assert!(
            subset.data.len() < 20_000,
            "el subconjunto debería ser pequeño, es {} bytes",
            subset.data.len()
        );
        assert!(subset.gid_for_char.contains_key(&'H'));
        assert!(!subset.data.is_empty());
    }
}
