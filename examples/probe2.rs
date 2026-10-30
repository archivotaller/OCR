//! Prueba cargar/compilar el ONNX sin fijar la fact de entrada sintáctica,
//! dejando que tract resuelva los símbolos con el tensor concreto del run.
//!
//! Uso: cargo run --release --example probe2 -- <model.onnx> [b h w]

use std::io::Cursor;
use tract_onnx::prelude::*;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().ok_or_else(|| anyhow::anyhow!("uso: probe2 <model.onnx> [b h w]"))?;
    let b: usize = args.get(1).map(|s| s.parse().unwrap()).unwrap_or(1);
    let h: usize = args.get(2).map(|s| s.parse().unwrap()).unwrap_or(64);
    let w: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(64);

    let bytes = std::fs::read(path)?;
    println!("== {path} ==");
    let model = tract_onnx::onnx().model_for_read(&mut Cursor::new(&bytes))?;
    println!("  inputs: {:?}", model.inputs);
    println!("  outputs: {:?}", model.outputs);

    let optimized = model.into_optimized()?;
    let plan = optimized.into_runnable()?;

    let shape = [b, 3, h, w];
    let n = shape.iter().product::<usize>();
    let input = Tensor::from_shape(&shape, &vec![0.5f32; n])?;
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