//! Geometría compartida entre la entrada (píxeles) y la salida (puntos PDF).

/// Transformación afín `(x, y) -> (a·x + c·y + e, b·x + d·y + f)`.
///
/// Se usa para pasar de coordenadas de la imagen rasterizada (y hacia abajo) a
/// coordenadas de la página PDF (y hacia arriba), incluyendo rotaciones y
/// recortes del PDF de origen.
#[derive(Debug, Clone, Copy)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    /// Página creada a partir de una imagen: `img_w × img_h` píxeles ocupan
    /// `page_w × page_h` puntos, con el eje Y invertido.
    pub fn image_to_page(img_w: u32, img_h: u32, page_w: f32, page_h: f32) -> Self {
        Affine {
            a: page_w / img_w.max(1) as f32,
            b: 0.0,
            c: 0.0,
            d: -page_h / img_h.max(1) as f32,
            e: 0.0,
            f: page_h,
        }
    }

    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Transformación inversa, si la original no es singular.
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-9 {
            return None;
        }
        Some(Affine {
            a: self.d / det,
            b: -self.b / det,
            c: -self.c / det,
            d: self.a / det,
            e: (self.c * self.f - self.d * self.e) / det,
            f: (self.b * self.e - self.a * self.f) / det,
        })
    }

    /// Longitud en puntos de un vector de `n` píxeles a lo largo de X.
    pub fn scale_x(&self) -> f32 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    /// Longitud en puntos de un vector de `n` píxeles a lo largo de Y.
    pub fn scale_y(&self) -> f32 {
        (self.c * self.c + self.d * self.d).sqrt()
    }
}

pub type Quad = [[f32; 2]; 4];

/// Rectángulo envolvente de un cuadrilátero: `(x0, y0, x1, y1)`.
pub fn quad_bounds(q: &Quad) -> (f32, f32, f32, f32) {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for [x, y] in q {
        x0 = x0.min(*x);
        y0 = y0.min(*y);
        x1 = x1.max(*x);
        y1 = y1.max(*y);
    }
    (x0, y0, x1, y1)
}

/// Centro de un cuadrilátero.
pub fn quad_center(q: &Quad) -> (f32, f32) {
    let (mut x, mut y) = (0.0, 0.0);
    for [px, py] in q {
        x += px;
        y += py;
    }
    (x / 4.0, y / 4.0)
}

/// Expande el cuadrilátero `margin` unidades hacia fuera por cada lado.
///
/// Se trabaja en el sistema local del cuadrilátero (dirección del borde
/// superior y perpendicular), de modo que una línea ancha y baja crece también
/// en vertical. Con cuadriláteros degenerados se usa la caja envolvente.
pub fn expand_quad(q: &Quad, margin: f32) -> Quad {
    if margin <= 0.0 {
        return *q;
    }
    let (cx, cy) = quad_center(q);
    let (tux, tuy) = (q[1][0] - q[0][0], q[1][1] - q[0][1]);
    let (tvx, tvy) = (q[3][0] - q[0][0], q[3][1] - q[0][1]);
    let lu = (tux * tux + tuy * tuy).sqrt();
    let lv = (tvx * tvx + tvy * tvy).sqrt();
    if lu < 1e-3 || lv < 1e-3 {
        let (x0, y0, x1, y1) = quad_bounds(q);
        return rect_quad(x0 - margin, y0 - margin, x1 + margin, y1 + margin);
    }
    let (ux, uy) = (tux / lu, tuy / lu);
    let (vx, vy) = (tvx / lv, tvy / lv);

    let mut half_u = 0.0f32;
    let mut half_v = 0.0f32;
    for [x, y] in q {
        let dx = x - cx;
        let dy = y - cy;
        half_u = half_u.max((dx * ux + dy * uy).abs());
        half_v = half_v.max((dx * vx + dy * vy).abs());
    }
    let (au, av) = (half_u + margin, half_v + margin);
    let corner = |su: f32, sv: f32| {
        [
            cx + su * au * ux + sv * av * vx,
            cy + su * au * uy + sv * av * vy,
        ]
    };
    [
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ]
}

/// Punto dentro de un cuadrilátero convexo (o de su envolvente si no lo es).
pub fn point_in_quad(x: f32, y: f32, q: &Quad) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (x1, y1) = (q[i][0], q[i][1]);
        let (x2, y2) = (q[(i + 1) % 4][0], q[(i + 1) % 4][1]);
        let cross = (x2 - x1) * (y - y1) - (y2 - y1) * (x - x1);
        if cross.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// Rectángulo como cuadrilátero (para el modo de borrado rectangular).
pub fn rect_quad(x0: f32, y0: f32, x1: f32, y1: f32) -> Quad {
    [[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

/// Intersección de dos rectángulos `(x0, y0, x1, y1)`.
pub fn intersect(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> Option<(f32, f32, f32, f32)> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = a.2.min(b.2);
    let y1 = a.3.min(b.3);
    if x1 > x0 && y1 > y0 {
        Some((x0, y0, x1, y1))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn afines_de_imagen_invierten_y() {
        let t = Affine::image_to_page(100, 50, 100.0, 50.0);
        assert_eq!(t.map(0.0, 0.0), (0.0, 50.0));
        assert_eq!(t.map(100.0, 50.0), (100.0, 0.0));
    }

    #[test]
    fn expandir_y_comprobar_punto() {
        let q = rect_quad(10.0, 10.0, 20.0, 20.0);
        let e = expand_quad(&q, 2.0);
        assert!(e[0][0] < 10.0);
        assert!(point_in_quad(15.0, 15.0, &e));
        assert!(!point_in_quad(40.0, 15.0, &e));
    }

    #[test]
    fn interseccion_de_rectangulos() {
        let a = (0.0, 0.0, 10.0, 10.0);
        let b = (5.0, 5.0, 20.0, 20.0);
        assert_eq!(intersect(a, b), Some((5.0, 5.0, 10.0, 10.0)));
        assert_eq!(intersect(a, (20.0, 20.0, 30.0, 30.0)), None);
    }
}
