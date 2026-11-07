# Historial de cambios

Formato basado en [Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/)
y versionado [SemVer](https://semver.org/lang/es/).

## [0.2.0] — `v0.2.0`

Consolidada en `main` el 2026-11-07 (desarrollada en
`feat/texto-visible-reemplazo`, integrada por *fast-forward*).

### Añadido

- **Entrada PDF** rasterizada con PDFium embebido en el ejecutable: lectura,
  rotación de página y reutilización del documento entre aperturas.
- **PDF con texto visible**: borra solo la zona de cada línea detectada y
  escribe encima texto real con la fuente incrustada, conservando imágenes y
  vectores del documento. Salida buscable, copiable y editable.
- **Plan de reemplazo** con borrado selectivo, cajas y línea base reales, y
  colores de fondo/texto muestreados del propio documento.
- **Fuentes incluidas** (Noto Sans/Serif) con sustitución de glifos CJK y
  generación de subconjuntos TrueType con anchos reales.
- **GUI**: apertura de PDF de entrada, vista previa del reemplazo y cuentagotas
  para elegir colores.
- **Informe por línea**: identifica la fuente de origen de cada línea de texto.
- **Tests** de salida visible: comparación raster, overlay y páginas rotadas.

### Corregido

- Reutilización del enlace de PDFium entre aperturas sucesivas.

## [0.1.0] — `v0.1.0`

Primer hito público.

### Añadido

- Pipeline OCR con PP-OCRv6 sobre `tract`, en tiers `tiny`, `small` y `medium`.
- CLI para imágenes, carpetas y lotes recursivos; salidas TXT, Markdown, JSON
  y PDF buscable.
- OCR con hilos configurables, soporte TIFF completo (1/2/4/8/16 bits, CMYK,
  multipágina) y PDF de varias páginas.
- GUI con worker en segundo plano, lotes y documentos multipágina.
- Tests E2E de OCR, PDF, CLI y recursos del sistema.
- README completo y licencia Apache-2.0.
