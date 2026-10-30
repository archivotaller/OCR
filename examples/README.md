# Herramientas de diagnóstico ONNX (histórico)

Estos ejemplos son las sondas que se usaron durante la fase de I+D para
conseguir que `tract` cargara los ONNX de PP-OCRv6. **No forman parte del
producto** y no se compilan en un `cargo build`/`cargo test` normal: están
detrás de la feature `dev-tools`.

## Por qué existían

Los `inference.onnx` oficiales exportan formas con símbolos y expresiones
(`DynamicDimension.N`, `floor(...)`, `ceil(...)`) que el *shape inference* de
`tract` no sabe resolver. La solución final fue usar los ONNX re-exportados del
CDN (ver `src/models.rs`) fijando la `fact` de entrada concreta por tamaño, así
que estas herramientas ya no son necesarias para usar `octxt`.

## Uso

```bash
cargo run --release --features dev-tools --example scan  -- modelo.onnx
cargo run --release --features dev-tools --example expr  -- modelo.onnx
cargo run --release --features dev-tools --example sanitize -- in.onnx [out.onnx]
cargo run --release --features dev-tools --example rewrite  -- in.onnx [out.onnx]
cargo run --release --features dev-tools --example probe  -- det.onnx [rec.onnx]
cargo run --release --features dev-tools --example probe2 -- modelo.onnx [b h w]
```

Se conservan como referencia; no se garantiza que sigan funcionando con
versiones futuras de `tract`.
