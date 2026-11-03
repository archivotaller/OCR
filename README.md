# octxt

OCR de escritorio y de línea de comandos basado en **PP-OCRv6** y
[`tract`](https://github.com/sonos/tract): 100 % Rust, sin Python ni runtime
externo. Incluye CLI, GUI y salidas TXT, JSON y **PDF buscable** con capa de
texto invisible.

- **Detección + reconocimiento** PP-OCRv6 en tres tamaños (`tiny`, `small`, `medium`).
- **CLI** para imágenes sueltas, carpetas completas o lotes recursivos.
- **GUI** con zoom, cajas superpuestas, varios documentos, TIFF multipágina y
  procesamiento por lotes en segundo plano.
- **PDF buscable** de una o varias páginas (una única fuente Unicode/ToUnicode).
- **TIFF real de escaneo**: 1/2/4/8/16 bits, gris, RGB/RGBA, CMYK/CMYKA y
  multipágina.
- Modelos descargados una sola vez, verificados con SHA-256 y cacheados:
  después funciona sin red.

## Requisitos

- Rust estable (probado con 1.92).
- Windows, Linux o macOS para la CLI. La GUI usa `eframe`/`glow` y funciona en
  los tres sistemas; el test de recursos del sistema está limitado a Windows.
- ~100 MB de disco para el tier `medium` (los modelos se descargan la primera vez).

## Compilación

```bash
cargo build --release
```

Binarios:

- `target/release/octxt` — CLI.
- `target/release/octxt-gui` — aplicación de escritorio.

## Uso rápido (CLI)

```bash
# Texto a stdout
octxt --tier tiny txt foto.jpg

# Un .txt por imagen
octxt --tier small txt --out-dir salida imagenes/

# JSON con cajas y confianzas
octxt --tier tiny txt --json documento.png

# PDF buscable (un solo archivo; si es TIFF multipágina, un PDF multipágina)
octxt --tier medium pdf --out documento.pdf escaneo.tif

# Cajas en TSV: archivo, cuadrilátero, det, rec, texto
octxt boxes --tier tiny foto.jpg
```

`--tier` se puede pasar antes o después del subcomando:

| Tier | Detección | Reconocimiento | Uso |
|---|---|---|---|
| `tiny` | 1,7 MB | 4,3 MB | Pruebas rápidas, imágenes limpias |
| `small` | 9,5 MB | 20 MB | Equilibrio calidad/velocidad |
| `medium` | 59 MB | 73 MB | Documentos difíciles (por defecto) |

### Opciones globales útiles

```text
--tier <tiny|small|medium>   Modelo PP-OCRv6
--threads <N>                Hilos de cómputo (por defecto, todos los lógicos)
--model-dir <DIR>            Caché de modelos alternativa
--det-thresh <F>             Umbral de binarización de detección (0.2)
--box-thresh <F>             Confianza mínima de caja (0.45)
--unclip-ratio <F>           Expansión de cajas (1.4)
--limit-side-len <N>         Lado de referencia de detección (960)
--min-rec-score <F>          Descarta líneas con poca confianza (0.3)
--split-words                Reconoce palabra por palabra (recupera espacios)
--plan-cache <N>             Caché de planes compilados de tract (16)
--no-prewarm                 No compila planes al arrancar
```

### Gestión de modelos

```bash
octxt --tier tiny fetch    # descarga y verifica
octxt --tier tiny check    # comprueba SHA-256 de la caché
```

Caché por defecto:

- Windows: `%LOCALAPPDATA%\octxt\models`
- Linux/macOS: `~/.cache/octxt/models`

También se puede forzar con `--model-dir` o con la variable `OCTXT_CACHE_DIR`.

## GUI

```bash
octxt-gui
octxt-gui --tier tiny foto.jpg escaneo.tif
octxt-gui --tier small --ocr carpeta/
```

- Abre imágenes sueltas, **varios archivos**, carpetas o arrastra y suelta.
- `Ctrl+O`: abrir archivos.
- `Ctrl+Enter`: OCR de la página actual.
- `Ctrl+Shift+Enter`: OCR de todos los documentos abiertos.
- Botones **OCR doc** y **OCR todo** para lotes; **Vaciar cola** cancela lo pendiente.
- El motor y el OCR corren en un hilo dedicado: la interfaz no se bloquea.
- Selector de documento y de página para TIFF/PDF multipágina.
- Exporta **TXT** y **JSON** de todo lo abierto y **PDF** buscable:
  un solo documento → un PDF; varios → un PDF por documento en la carpeta elegida.
- `--ocr` lanza el OCR automáticamente al terminar de cargar el motor (útil para
  automatización y pruebas).

Formatos de entrada: `png`, `jpg`, `jpeg`, `bmp`, `gif`, `webp`, `tif`, `tiff`,
`pnm`.

## Salidas

### TXT / Markdown

Una línea de texto por región detectada, en orden de lectura. `--md` añade una
cabecera con el nombre del archivo.

### JSON

```json
[
  {
    "file": "foto.jpg",
    "width": 1280,
    "height": 960,
    "elapsed_ms": 1120,
    "lines": [
      {
        "points": [[10.0, 20.0], [300.0, 20.0], [300.0, 60.0], [10.0, 60.0]],
        "text": "DANGER!",
        "det_score": 0.93,
        "rec_score": 0.97
      }
    ]
  }
]
```

### PDF buscable

La imagen de cada página se incrusta a página completa (reutilizando el JPEG
original si lo es) y encima se dibuja el texto OCR con modo de render invisible
(`Tr 3`). El texto se puede seleccionar, copiar y buscar. Todas las páginas
comparten una fuente `Type0`/`Identity-H` con su CMap `ToUnicode`, de modo que
acentos y Unicode se conservan.

## Arquitectura

```text
src/
├── lib.rs                 API pública
├── main.rs                entrada de la CLI
├── cli.rs                 argumentos, lotes y salidas
├── models.rs              manifiesto, descarga y caché SHA-256
├── image_io.rs            carga de imágenes y TIFF (bits, CMYK, multipágina)
├── dict.rs                diccionario del reconocedor
├── ocr/
│   ├── engine.rs          pipeline detección → orden → reconocimiento paralelo
│   ├── preprocess.rs      normalización de la red de detección
│   ├── postprocess.rs     DBPostProcess (cajas, puntuaciones)
│   ├── geometry.rs        cuadriláteros, unclip, recorte rectificado
│   ├── recognize.rs       CTC, charset, preproceso de tiras
│   └── config.rs          parámetros de detección/normalización
├── output/
│   ├── common.rs          FileResult
│   ├── txt.rs             texto plano / Markdown
│   ├── json.rs            serialización con cajas
│   └── pdf.rs             PDF buscable multipágina
└── bin/octxt-gui/main.rs  GUI eframe/egui con worker y cola de OCR
```

El reconocimiento por caja es independiente y se ejecuta en paralelo con
`rayon`. `--threads N` crea un pool dedicado de exactamente `N` hilos; sin la
opción se usa el pool global (todos los núcleos lógicos).

## Tests

```bash
cargo test                     # unitarios + integración
cargo test --test ocr_e2e      # OCR real sobre un TIFF descargado
cargo test --test pdf          # PDF de 1 y 2 páginas (validado con lopdf)
cargo test --test cli          # binario real: --version, txt, json, pdf
cargo test --test system_resources -- --nocapture
```

- Los tests E2E usan `tests/fixtures/eurotext.tif` y los modelos cacheados.
  El tier se elige con `OCTXT_TEST_TIER` (`tiny` por defecto):
  `OCTXT_TEST_TIER=small cargo test --test ocr_e2e`.
- `tests/image_io.rs` cubre TIFF de 1 bit, CMYK, 16 bits y multipágina.
- `tests/system_resources.rs` (solo Windows) ejecuta el OCR completo y mide con
  la API del sistema el uso **real** de hardware: CPU lógica, hilos del pool,
  tiempo, CPU del proceso, CPU global, RSS actual y pico, fallos de página y RAM
  total/disponible. Ejemplo de salida en una máquina de 16 hilos:

```text
=== Informe de recursos (hardware real) ===
CPU lógica disponible : 16
Hilos del pool OCR    : 16
Páginas procesadas    : 1
Líneas OCR            : 12
Tiempo pared          : 38.83 s
CPU del proceso       : 71.44 s (184.0 % de un núcleo)
CPU del sistema       : 20.6 % (idle 493.27 s / busy 128.14 s)
RSS pico del proceso  : 220.9 MiB
RAM del sistema       : 15.2 GiB total, 5.6 GiB libre (63 % en uso)
```

Las cifras dependen de la máquina; los asserts solo comprueban que hubo uso real
de CPU y memoria, que el OCR produjo texto y que el sistema reporta valores
coherentes.

## Herramientas de desarrollo

`examples/` contiene sondas históricas de ONNX/tract usadas durante la
investigación. No se compilan por defecto; están detrás de la feature
`dev-tools`:

```bash
cargo run --release --features dev-tools --example scan -- modelo.onnx
```

Ver `examples/README.md`.

## Licencia

Apache-2.0. Ver [`LICENSE`](LICENSE).

El pipeline y los parámetros están basados en
[`gradan-hash/paddle-paddle-v6-ocr-rust`](https://github.com/gradan-hash/paddle-paddle-v6-ocr-rust)
(Apache-2.0). Los ONNX de PP-OCRv6 se descargan del CDN de ese proyecto. Los
fixtures de test provienen de
[`tesseract-ocr/test`](https://github.com/tesseract-ocr/test) (Apache-2.0); el
resto se genera con Pillow y es de dominio público (ver
`tests/fixtures/README.md`).
