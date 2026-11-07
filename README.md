# octxt

OCR de escritorio y de línea de comandos basado en **PP-OCRv6** y
[`tract`](https://github.com/sonos/tract): 100 % Rust, sin Python ni runtime
externo. Incluye CLI, GUI y salidas TXT, JSON y dos variantes de PDF: **PDF
buscable** con capa de texto invisible y **PDF con texto visible** que borra el
original y escribe encima el texto del OCR (buscable, copiable y editable).

- **Detección + reconocimiento** PP-OCRv6 en tres tamaños (`tiny`, `small`, `medium`).
- **CLI** para imágenes sueltas, carpetas completas o lotes recursivos.
- **GUI** con zoom, cajas superpuestas, varios documentos, TIFF multipágina,
  PDF, vista previa del reemplazo y procesamiento por lotes en segundo plano.
- **PDF buscable** de una o varias páginas (una única fuente Unicode/ToUnicode).
- **PDF con texto visible**: borra solo la zona de cada línea detectada y
  superpone texto real con la fuente incrustada, conservando el resto del
  documento (imágenes y vectores).
- **Entrada PDF** rasterizada con **PDFium embebido** en el ejecutable.
- **TIFF real de escaneo**: 1/2/4/8/16 bits, gris, RGB/RGBA, CMYK/CMYKA y
  multipágina.
- Modelos descargados una sola vez, verificados con SHA-256 y cacheados:
  después funciona sin red.

## Estado del proyecto

- **`main`** refleja la versión **0.2.0**, etiquetada como `v0.2.0`.
- La rama `feat/texto-visible-reemplazo`, donde se desarrolló la 0.2.0, se
  consolidó en `main` mediante *fast-forward* el 2026-11-07. No se perdió
  ningún commit: la rama se conserva como referencia histórica y apunta al
  mismo commit que `main`.
- `v0.1.0` marca el primer hito público (OCR, CLI, GUI y PDF buscable).
- El detalle por versión está en [`CHANGELOG.md`](CHANGELOG.md).

## Requisitos

- Rust estable (probado con 1.92).
- Windows, Linux o macOS para la CLI. La GUI usa `eframe`/`glow` y funciona en
  los tres sistemas; el test de recursos del sistema está limitado a Windows.
- ~100 MB de disco para el tier `medium` (los modelos se descargan la primera vez).
- Para leer PDF de entrada no hace falta instalar nada: **PDFium** viaja
  embebido en el ejecutable (se descarga y verifica una vez durante la
  compilación y se extrae a la caché local al arrancar).

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

# PDF con el texto visible: borra el original y escribe encima el del OCR
octxt --tier medium pdf --replace-text --out documento.visible.pdf escaneo.tif

# PDF de entrada: se rasteriza, se reconoce y se reemplaza el texto
octxt pdf --replace-text --dpi 300 --out salida.pdf original.pdf

# Excluir cajas concretas (página:línea, 1-based) y fijar colores
octxt pdf --replace-text --exclude 1:3 --exclude 2:1 \
  --background '#ffffff' --text-color '#111111' original.pdf

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
- Panel **Texto visible** para el PDF con reemplazo: modo de borrado (real o
  visual), margen, colores automáticos o fijados con cuentagotas, fuente latina
  y CJK, DPI de rasterizado y casillas para **excluir cajas** del reemplazo.
  La vista previa muestra el parche y el texto nuevo antes de exportar.
- `--ocr` lanza el OCR automáticamente al terminar de cargar el motor (útil para
  automatización y pruebas).

Formatos de entrada: `pdf`, `png`, `jpg`, `jpeg`, `bmp`, `gif`, `webp`, `tif`,
`tiff`, `pnm`.

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

### PDF con texto visible

`pdf --replace-text` (o el botón **PDF visible** de la GUI) genera
`{nombre}.visible.pdf`: borra únicamente la zona de cada línea detectada y
escribe encima el texto del OCR con una fuente real incrustada, de forma que se
puede seleccionar, copiar y **editar** en un visor PDF. El resto del documento
no se toca.

```bash
octxt pdf --replace-text --out salida.pdf entrada.pdf
```

| Opción | Efecto |
|---|---|
| `--erase-mode real\|visual` | `real` pinta los píxeles de la zona; `visual` deja la imagen intacta y tapa con una capa opaca. |
| `--erase-margin <F>` | Margen alrededor de cada caja, como fracción de su altura (0.3). |
| `--background <#rrggbb\|auto>` | Color del parche. `auto` muestrea el fondo de cada caja. |
| `--text-color <#rrggbb\|auto>` | Color del texto. `auto` busca contraste con el fondo. |
| `--font`, `--font-index` | Fuente TTF/OTF para latín (por defecto Noto Sans incluida; `--font-serif` usa Noto Serif). |
| `--font-cjk`, `--font-cjk-index` | Fuente para caracteres CJK (por defecto, una del sistema). |
| `--dpi <N>` | Resolución de rasterizado de los PDF de entrada (300). |
| `--exclude <página:línea>` | No reemplaza esa caja; se puede repetir. |

Cómo se comporta:

- **Páginas vectoriales de un PDF**: se trabaja en **overlay** obligatorio. Se
  conserva el content stream original (vectores, imágenes, texto) y se añade
  encima el parche y la capa de texto nueva. Nunca se aplana la página.
- **Páginas escaneadas** (sin texto ni vectores, rotación 0) e **imágenes/TIFF**:
  se reconstruye la página con el raster sin pérdida (PNG → Flate) y el borrado
  es real o visual según `--erase-mode`.
- **Páginas con rotación** (`/Rotate`): siempre overlay, para no alterar la
  geometría de la página.
- Si una caja tiene fondo no uniforme se rellena con un color sólido muestreado
  del entorno; el informe lo indica.
- El informe y la GUI muestran la **fuente de origen** de cada línea (familia,
  estilo y tamaño). Esa fuente se usa para calcular tamaño y línea base; el
  texto nuevo se escribe con la fuente elegida (`--font`, `--font-serif` o
  `--font-cjk`, o los botones equivalentes de la GUI).
- Si un carácter no existe en la fuente elegida ni en las de reserva, esa línea
  **no se borra** y se reporta en lugar de escribir texto incompleto.

Limitaciones conocidas:

- En overlay el texto original sigue presente bajo el parche. Al extraer el
  texto del PDF aparecen las dos capas (original + OCR); es el precio de no
  destruir el contenido vectorial.
- El reemplazo se guía por las cajas del OCR: líneas no detectadas se quedan
  como estaban.
- El texto nuevo usa la fuente elegida, no la del documento original, así que
  puede haber pequeñas diferencias métricas aunque el tamaño y la línea base se
  toman del PDF cuando están disponibles.

## Arquitectura

```text
src/
├── lib.rs                 API pública
├── main.rs                entrada de la CLI
├── cli.rs                 argumentos, lotes y salidas
├── models.rs              manifiesto, descarga y caché SHA-256
├── image_io.rs            carga de imágenes y TIFF (bits, CMYK, multipágina)
├── geom.rs                transformaciones afines y geometría de cuadriláteros
├── dict.rs                diccionario del reconocedor
├── ocr/
│   ├── engine.rs          pipeline detección → orden → reconocimiento paralelo
│   ├── preprocess.rs      normalización de la red de detección
│   ├── postprocess.rs     DBPostProcess (cajas, puntuaciones)
│   ├── geometry.rs        cuadriláteros, unclip, recorte rectificado
│   ├── recognize.rs       CTC, charset, preproceso de tiras
│   └── config.rs          parámetros de detección/normalización
├── input/
│   └── pdf.rs             PDFium: rasterizado, rotación y fuentes de origen
├── font/
│   ├── mod.rs             resolución de fuentes (latín, serif, CJK, reservas)
│   └── subset.rs          subconjuntos TrueType con anchos reales
├── replace/
│   ├── plan.rs            plan de borrado/texto con caja y línea base reales
│   └── color.rs           muestreo de fondo/texto y borrado de píxeles
├── output/
│   ├── common.rs          FileResult
│   ├── txt.rs             texto plano / Markdown
│   ├── json.rs            serialización con cajas
│   ├── pdf.rs             PDF buscable multipágina
│   └── visible.rs         PDF con texto visible (overlay y raster sin pérdida)
└── bin/octxt-gui/main.rs  GUI eframe/egui con worker, cola y vista previa
```

El reconocimiento por caja es independiente y se ejecuta en paralelo con
`rayon`. `--threads N` crea un pool dedicado de exactamente `N` hilos; sin la
opción se usa el pool global (todos los núcleos lógicos).

## Tests

```bash
cargo test                     # unitarios + integración
cargo test --test ocr_e2e      # OCR real sobre un TIFF descargado
cargo test --test pdf          # PDF de 1 y 2 páginas (validado con lopdf)
cargo test --test visible      # PDF con texto visible: raster, visual y overlay
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
