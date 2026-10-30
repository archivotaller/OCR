//! Sonda: verifica que tract carga/compila/ejecuta los ONNX de PP-OCRv6 y
//! reporta el rango de la salida (para decidir si hay que aplicar sigmoide).
//!
//! Uso: cargo run --release --example probe -- <det.onnx> [rec.onnx]

use std::io::Cursor;
use tract_onnx::prelude::*;

fn run_one(path: &str, shape: [usize; 4]) -> anyhow::Result<()> {
    let bytes = std::fs::read(path)?;
    println!("== {path} ({} bytes) ==", bytes.len());

    let plan = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(&bytes))?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()?;

    let n = shape.iter().product::<usize>();
    let input = tract_onnx::prelude::Tensor::from_shape(&shape, &vec![0.5f32; n])?;
    let result = plan.run(tvec!(input.into()))?;
    for (i, t) in result.iter().enumerate() {
        let v = t.to_array_view::<f32>()?;
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        let mut sum = 0.0f64;
        for &x in v.iter() {
            min = min.min(x);
            max = max.max(x);
            sum += x as f64;
        }
        println!(
            "  salida[{i}] forma={:?} min={min:.4} max={max:.4} mean={:.4}",
            v.shape(),
            sum / v.len() as f64
        );
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("uso: probe <det.onnx> [rec.onnx] [H W]");
        std::process::exit(2);
    }
    let h: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(64);
    let w: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(64);
    run_one(&args[0], [1, 3, h, w])?;
    if let Some(rec) = args.get(1) {
        run_one(rec, [1, 3, 48, 320])?;
    }
    Ok(())
}
