# Fixtures de test

| Archivo | Origen | Licencia | Para qué |
|---|---|---|---|
| `eurotext.tif` | [tesseract-ocr/test](https://github.com/tesseract-ocr/test/blob/main/testing/eurotext.tif) | Apache-2.0 (repo de tests de Tesseract) | Escaneo TIFF de 1 bit con texto en inglés, alemán, francés, italiano, español y portugués. Prueba de carga TIFF + OCR E2E. |
| `cmyk_2x2.tif` | Generado con Pillow 12 | Dominio público | 2×2 píxeles CMYK puros (rojo, verde, azul, blanco) para validar la conversión TIFF CMYK → RGB. |
| `multipage_2.tif` | Generado con Pillow 12 | Dominio público | Dos páginas de 32×32 (blanca y negra) para validar TIFF multipágina. |
| `gray16.tif` | Generado con Pillow 12 | Dominio público | Gradiente gris de 16 bits para validar la reducción al byte alto. |

Los archivos generados se crearon con este script:

```python
from PIL import Image

p1 = Image.new("L", (32, 32), 255)
p2 = Image.new("L", (32, 32), 0)
p1.save("multipage_2.tif", save_all=True, append_images=[p2], compression="raw")

im = Image.new("I;16", (16, 4))
im.putdata([i * 4369 for _ in range(4) for i in range(16)])
im.save("gray16.tif", compression="raw")
```
