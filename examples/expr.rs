//! Lista las cadenas de expresión de forma ("floor(...)"/"ceil(...)") que tract
//! intenta parsear como TDim.
//!
//! Uso: cargo run --release --example expr -- <model.onnx>

use std::collections::HashSet;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("uso: expr <model.onnx>"))?;
    let bytes = std::fs::read(input)?;

    // Localiza el inicio de cada string que contenga "floor(" o "ceil(",
    // escaneando a partir de la marca y retrocediendo hasta un byte no imprimible
    // (o hasta el prefijo de longitud).
    let mut set: HashSet<String> = HashSet::new();
    let mut i = 0usize;
    while i + 6 < bytes.len() {
        if (&bytes[i..i + 5] == b"ceil(") || (&bytes[i..i + 6] == b"floor(") {
            // retrocede al inicio de la cadena
            let mut s = i;
            while s > 0 && (bytes[s - 1] >= 0x20 && bytes[s - 1] <= 0x7E) {
                s -= 1;
            }
            let mut e = i + 5;
            while e < bytes.len() && (bytes[e] >= 0x20 && bytes[e] <= 0x7E || bytes[e] == b'\n') {
                e += 1;
            }
            let text = String::from_utf8_lossy(&bytes[s..e]).to_string();
            set.insert(text);
            i = e;
        } else {
            i += 1;
        }
    }

    let mut v: Vec<&String> = set.iter().collect();
    v.sort();
    for (n, t) in v.iter().enumerate() {
        println!("[{n}] {t}");
    }
    println!("total distintos: {}", v.len());
    Ok(())
}
