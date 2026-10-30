//! Reescribe "DynamicDimension.N" y las cadenas de expresión de forma
//! ("floor(...)"/...) de los ONNX PP-OCRv6 a una forma que tract pueda parsear
//! como TDim, validando cada reescritura con el parser real de tract y su
//! semántica numérica (misma longitud de bytes).
//!
//! Uso: cargo run --release --example rewrite -- <in.onnx> [out.onnx]

use anyhow::Context;
use std::collections::HashMap;
use tract_data::internal::DimLike;
use tract_data::prelude::*;

const PREFIX: &[u8] = b"DynamicDimension.";
const OUT_PREFIX: &[u8] = b"dynamicdimensions";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("uso: rewrite <in.onnx> [out.onnx]");
        std::process::exit(2);
    }
    let input = &args[0];
    let output = args
        .get(1)
        .map(|s| s.clone())
        .unwrap_or_else(|| input.trim_end_matches(".onnx").to_string() + ".sane.onnx");

    let bytes = std::fs::read(input)?;
    let (out, n, expr_s, expr_r) = rewrite(&bytes)?;
    if n == 0 {
        println!("{input}: sin cambios");
        return Ok(());
    }
    std::fs::write(&output, &out)?;
    println!("{input} -> {output}: {n} parches ({expr_s} expresiones, {expr_r} nombres)");
    Ok(())
}

fn is_printable(b: u8) -> bool {
    b >= 0x20 && b <= 0x7E
}

/// Nombre nuevo para un símbolo: "dynamicdimensions<dígitos>".
fn new_name(digits: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(OUT_PREFIX.len() + digits.len());
    v.extend_from_slice(OUT_PREFIX);
    v.extend_from_slice(digits);
    v
}

fn digits_after(b: &[u8], at: usize) -> anyhow::Result<(usize, Vec<u8>)> {
    let mut k = at;
    while k < b.len() && b[k].is_ascii_digit() {
        k += 1;
    }
    if k == at {
        anyhow::bail!("{PREFIX:?} sin dígitos en offset {at}");
    }
    Ok((k, b[at..k].to_vec()))
}

/// Reescribe todo el archivo. Devuelve (bytes, nº de parches, nº expr, nº nombres).
pub fn rewrite(bytes: &[u8]) -> anyhow::Result<(Vec<u8>, usize, usize, usize)> {
    if bytes.len() < PREFIX.len() {
        return Ok((bytes.to_vec(), 0, 0, 0));
    }
    let mut out = bytes.to_vec();
    let mut patches = 0usize;
    let mut n_expr = 0usize;
    let mut n_name = 0usize;
    let mut cache: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();

    let mut i = 1usize;
    while i + PREFIX.len() <= out.len() {
        if &out[i..i + PREFIX.len()] == PREFIX {
            let (k, digits) = digits_after(&out, i + PREFIX.len())?;
            let len_of = k - i;

            if !is_printable(out[i - 1]) && out[i - 1] as usize == len_of && out[i - 2] & 0x07 == 2 {
                // Nombre simbólico: string protobuf que empieza exactamente aquí.
                let nn = new_name(&digits);
                debug_assert_eq!(nn.len(), len_of);
                out[i..k].copy_from_slice(&nn);
                n_name += 1;
                patches += 1;
                i = k;
                continue;
            }

            // Expresión de forma: la cadena completa empieza con "floor("/"ceil(".
            let (s, len, _digits) = find_string_start(&out, i)?;
            let full = out[s..s + len].to_vec();
            debug_assert!(full.iter().all(|&b| is_printable(b)));
            let replacement = if let Some(r) = cache.get(&full) {
                r.clone()
            } else {
                let r = rewrite_expr(&full)?;
                cache.insert(full.clone(), r.clone());
                n_expr += 1;
                r
            };
            if replacement.len() != len {
                anyhow::bail!("reemplazo con longitud distinta ({}) para len {len}", replacement.len());
            }
            out[s..s + len].copy_from_slice(&replacement);
            patches += 1;
            i = s + len;
        } else {
            i += 1;
        }
    }
    Ok((out, patches, n_expr, n_name))
}

/// Localiza el inicio y la longitud de la cadena de expresión que contiene `i`.
/// Valida cada candidato con el análisis real de plantilla.
fn find_string_start(b: &[u8], i: usize) -> anyhow::Result<(usize, usize, String)> {
    let lo = i.saturating_sub(300);
    for j in (lo..=i).rev() {
        let is_floor = j + 6 <= b.len() && &b[j..j + 6] == b"floor(";
        let is_ceil = j + 5 <= b.len() && &b[j..j + 5] == b"ceil(";
        if !(is_floor || is_ceil) || j == 0 {
            continue;
        }
        let len = b[j - 1] as usize;
        if len < 20 || j + len > b.len() || !(j <= i && i < j + len) {
            continue;
        }
        let cand = &b[j..j + len];
        if !cand.iter().all(|&x| is_printable(x)) {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(cand) {
            if let Ok((_nfunc, digits)) = analyze_expr(text) {
                return Ok((j, len, digits));
            }
        }
    }
    anyhow::bail!("no localicé la cadena de expresión que contiene offset {i}")
}

/// Analiza la plantilla de una expresión de forma. Devuelve (nº de
/// floor(/ceil( anidados, dígitos del índice de dimensión).
fn analyze_expr(text: &str) -> anyhow::Result<(usize, String)> {
    let mut nfunc = 0usize;
    let mut rest = text;
    loop {
        if let Some(r) = rest.strip_prefix("floor(") {
            nfunc += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("ceil(") {
            nfunc += 1;
            rest = r;
        } else {
            break;
        }
    }
    if nfunc == 0 {
        anyhow::bail!("expresión sin floor(/ceil(: {text:?}");
    }
    let (sym, tail) = rest
        .split_once("/2 - 1/2)")
        .ok_or_else(|| anyhow::anyhow!("patrón interno inesperado en {text:?}"))?;
    let expected_tail = "/2)".repeat(nfunc - 1) + " + 1";
    if tail != expected_tail {
        anyhow::bail!(
            "forma inesperada en {text:?}: cola {tail:?} != {expected_tail:?}"
        );
    }
    let digits = sym
        .strip_prefix("DynamicDimension.")
        .or_else(|| sym.strip_prefix("dynamicdimensions"))
        .ok_or_else(|| anyhow::anyhow!("símbolo inesperado {sym:?} en {text:?}"))?;
    if digits.is_empty() || !digits.bytes().all(|b: u8| b.is_ascii_digit()) {
        anyhow::bail!("símbolo con dígitos no válidos: {sym:?}");
    }
    Ok((nfunc, digits.to_string()))
}

/// Valores numéricos que debe producir la expresión original (semántica real).
fn reference_values(func_floors: usize, x: i64) -> i64 {
    let mut v = (x as f64 / 2.0 - 0.5).floor() + 1.0;
    for _ in 1..func_floors {
        v = (v / 2.0).floor() + 1.0;
    }
    v as i64
}

/// Genera una expresión TDim válida del mismo largo que `orig` que evalúa al
/// valor de la expresión original para toda x >= 1.
fn rewrite_expr(orig: &[u8]) -> anyhow::Result<Vec<u8>> {
    let text = std::str::from_utf8(orig).context("expresión no es UTF-8")?;
    let (nfunc, digits) = analyze_expr(text)?;
    let new_sym = String::from_utf8(new_name(digits.as_bytes()))?;

    // genera la forma entera equivalente
    let mut expr = format!("({new_sym} + 1)/2");
    for _ in 1..nfunc {
        expr = format!("({expr} + 2)/2");
    }

    // rellena con espacios inter-token (la gramática los admite) hasta el largo.
    let r_bytes = expr.into_bytes();
    let pad = orig.len() as i64 - r_bytes.len() as i64;
    if pad < 0 {
        anyhow::bail!(
            "expansión {nfunc} no cabe en {orig:?} (necesita {}b, hay {})",
            orig.len(),
            r_bytes.len()
        );
    }
    // ubicación del símbolo (único) en `r_bytes` para insertar los espacios:
    let symb = new_sym.as_bytes();
    let at = r_bytes
        .windows(symb.len())
        .position(|w| w == symb)
        .ok_or_else(|| anyhow::anyhow!("símbolo {new_sym} ausente en {:?}", r_bytes))?;
    let mut r = Vec::with_capacity(r_bytes.len() + pad as usize);
    r.extend_from_slice(&r_bytes[..at]);
    r.extend_from_slice(symb);
    for _ in 0..pad {
        r.push(b' ');
    }
    r.extend_from_slice(&r_bytes[at + symb.len()..]);
    debug_assert_eq!(r.len(), orig.len());

    // valida con el parser real de tract y su semántica numérica
    let rtext = std::str::from_utf8(&r).unwrap();
    let table = SymbolScope::default();
    let sym = table.sym(&new_sym);
    let tdim = table
        .parse_tdim(rtext)
        .with_context(|| format!("parseando {rtext:?}"))?;
    for x in 1..=512i64 {
        let mut sv = SymbolValues::default();
        sv.set(&sym, x);
        let got = tdim.eval(&sv).to_isize()? as i64;
        let want = reference_values(nfunc, x);
        if got != want {
            anyhow::bail!(
                "reenombrado no coincide para x={x}: {text:?} -> {rtext:?} ({got} != {want})"
            );
        }
    }
    Ok(r)
}