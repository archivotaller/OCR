//! Sanea dimensiones simbólicas "DynamicDimension.N" (paddle2onnx) que tract
//! no puede parsear (el nombre contiene un punto).
//!
//! La sustitución es byte a byte con la MISMA longitud: "DynamicDimension." (17)
//! + dígitos se reemplaza por "dynamicdimensions" (17) + los mismos dígitos.
//! Así el prefijo de longitud del protobuf sigue siendo válido y el resto del
//! archivo queda intacto.
//!
//! Uso: cargo run --release --example sanitize -- <in.onnx> [out.onnx]

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("uso: sanitize <in.onnx> [out.onnx]");
        std::process::exit(2);
    }
    let input = &args[0];
    let output = args
        .get(1)
        .map(|s| s.clone())
        .unwrap_or_else(|| input.trim_end_matches(".onnx").to_string() + ".sane.onnx");

    let bytes = std::fs::read(input)?;
    let (out, n) = sanitize(&bytes)?;
    if n == 0 {
        println!("{input}: sin cambios");
        return Ok(());
    }
    std::fs::write(&output, &out)?;
    println!(
        "{input} -> {output} ({n} dimensiones renombradas; {} bytes -> {} bytes)",
        bytes.len(),
        out.len()
    );
    Ok(())
}

pub const PREFIX: &[u8] = b"DynamicDimension.";
const OUT_PREFIX: &[u8] = b"dynamicdimensions";

/// Reemplaza cada ocurrencia de "DynamicDimension.<dígitos>" por un identificador
/// válido de igual longitud, preservando las longitudes varint del protobuf.
/// Devuelve el contenido saneado y el número de reemplazos.
pub fn sanitize(bytes: &[u8]) -> anyhow::Result<(Vec<u8>, usize)> {
    if bytes.len() < PREFIX.len() {
        return Ok((bytes.to_vec(), 0));
    }

    let mut out = bytes.to_vec();
    let mut changed = 0usize;
    let mut i = 1usize;
    while i + PREFIX.len() <= out.len() {
        if &out[i..i + PREFIX.len()] == PREFIX {
            let k = end_of_number(&out, i + PREFIX.len())?;
            check_context(&out, i, k)?;

            let mut newname = Vec::with_capacity(PREFIX.len() + (k - i - PREFIX.len()));
            newname.extend_from_slice(OUT_PREFIX);
            newname.extend_from_slice(&out[i + PREFIX.len()..k]);
            debug_assert_eq!(newname.len(), k - i);
            out[i..k].copy_from_slice(&newname);
            changed += 1;
            i = k;
        } else {
            i += 1;
        }
    }
    Ok((out, changed))
}

/// Índice justo después de la secuencia de dígitos que sigue al prefijo.
fn end_of_number(b: &[u8], j: usize) -> anyhow::Result<usize> {
    let mut k = j;
    while k < b.len() && b[k].is_ascii_digit() {
        k += 1;
    }
    if k == j {
        anyhow::bail!("'{:?}' sin dígitos en offset {}", PREFIX, j - PREFIX.len());
    }
    Ok(k)
}

/// Verifica que la ocurrencia esté en contexto de string (no en un blob binario).
fn check_context(b: &[u8], i: usize, k: usize) -> anyhow::Result<()> {
    let prev = b[i - 1];
    let printable = prev >= 0x20 && prev <= 0x7E || prev == b'\t';
    if printable {
        // Texto dentro de un string más largo ("floor(DynamicDimension.2/2 ...").
        let after = b[k];
        if after < 0x20 && after != b'\t' && after != b'\n' && after != b'\r' {
            anyhow::bail!(
                "byte binario {after:#04x} tras el número en {i}; ¿falso positivo en blob binario?"
            );
        }
        return Ok(());
    }
    // Prefijo de longitud varint (1 byte) que cubra este string, precedido de tag wiretype 2.
    let len = prev as usize;
    if len & 0x80 == 0 && len >= k - i && i >= 2 && b[i - 2] & 0x07 == 0x02 {
        return Ok(());
    }
    anyhow::bail!(
        "el prefijo {prev:#04x} antes del patrón en {i} no parece longitud de string; abandono"
    )
}