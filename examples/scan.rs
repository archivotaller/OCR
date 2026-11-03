//! Escanea un .onnx y reporta cada string protobuf que contenga el patrón
//! "DynamicDimension." (para decidir la estrategia de saneado).
//!
//! Uso: cargo run --release --example scan -- <model.onnx>

use sanitize_example::PREFIX;
use std::collections::HashMap;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let input = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("uso: scan <model.onnx>"))?;
    let bytes = std::fs::read(input)?;

    let mut found = 0usize;
    let mut kinds: HashMap<usize, usize> = HashMap::new();
    let mut printed = 0usize;
    let mut i = 0usize;
    while i + PREFIX.len() <= bytes.len() {
        if &bytes[i..i + PREFIX.len()] == PREFIX {
            found += 1;
            let lenb = if i >= 1 { bytes[i - 1] as usize } else { 0 };
            *kinds.entry(lenb).or_insert(0) += 1;
            if printed < 120 {
                printed += 1;
                let tag = if i >= 2 { bytes[i - 2] } else { 0 };
                let show = std::cmp::min(lenb.max(0), 160usize);
                let s = String::from_utf8_lossy(&bytes[i..(i + show)]);
                println!("offset={i:>9} len={lenb:>3} tag={tag:#04x} :: {s:?}");
            }
        }
        i += 1;
    }
    println!("total={found}");
    for (len, cnt) in &kinds {
        println!("  length {len}: {cnt} ocurrencias");
    }
    Ok(())
}

mod sanitize_example {
    pub const PREFIX: &[u8] = b"DynamicDimension.";
}
