//! octxt-gui: interfaz de escritorio del OCR (eframe/egui 0.35).
//!
//! MVP completo: abrir/arrastrar una imagen, visualizarla con las cajas
//! detectadas, ejecutar OCR, revisar resultados y exportar TXT/JSON/PDF.

use octxt::image_io;
use octxt::models::Tier;
use octxt::ocr::engine::{Engine, EngineConfig, OcrLine};
use octxt::output::{common::FileResult, json, pdf, txt};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;

// ---------------------------------------------------------------------------

const APP_ID: &str = "octxt-gui";

#[derive(Clone, Copy, PartialEq, Eq)]
enum GuiTier {
    Tiny,
    Small,
    Medium,
}

impl GuiTier {
    fn as_tier(self) -> Tier {
        match self {
            GuiTier::Tiny => Tier::Tiny,
            GuiTier::Small => Tier::Small,
            GuiTier::Medium => Tier::Medium,
        }
    }

    fn label(self) -> &'static str {
        match self {
            GuiTier::Tiny => "tiny (rápido)",
            GuiTier::Small => "small",
            GuiTier::Medium => "medium (preciso)",
        }
    }
}

/// Mensaje del hilo de carga del motor.
enum LoadMsg {
    Progress(String),
    Ready(Result<Engine, String>),
}

// ---------------------------------------------------------------------------

struct OcrtApp {
tier: GuiTier,
    prev_tier: GuiTier,

    /// Estado del motor (None = aún no pedido).
    engine: Option<Engine>,
    engine_log: Vec<String>,
    load_rx: Option<Receiver<LoadMsg>>,
    loaded_engine: bool,
    /// Tier con el que se pidió el motor (para no re-cargar si no cambia).
    engine_tier: GuiTier,

    path: Option<PathBuf>,
    pages: Vec<image::DynamicImage>,
    page: usize,
    texture: Option<egui::TextureHandle>,
    orig_bytes: Option<Vec<u8>>,

    /// Resultados por página (indexados como `pages`).
    results: Vec<Vec<OcrLine>>,
    elapsed_ms: Vec<u128>,
    selected: Option<usize>,

    // Vista de imagen.
    zoom: f32,
    pan: egui::Vec2,

    status: String,
    drop_hover: bool,
}

impl OcrtApp {
    fn new(ctx: &egui::Context) -> Self {
        setup_cjk_fonts(ctx);
        let mut app = OcrtApp {
            tier: GuiTier::Medium,
            prev_tier: GuiTier::Medium,
            engine: None,
            engine_log: Vec::new(),
            load_rx: None,
            loaded_engine: false,
            engine_tier: GuiTier::Medium,
            path: None,
            pages: Vec::new(),
            page: 0,
            texture: None,
            orig_bytes: None,
            results: Vec::new(),
            elapsed_ms: Vec::new(),
            selected: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            status: "Arrastra una imagen aquí o usa 'Abrir'…".into(),
            drop_hover: false,
        };
        app.request_engine_load();
        app
    }

    /// (Re)lanza la carga del motor en un hilo si el tier cambió o aún no se pidió.
    fn request_engine_load(&mut self) {
        if self.load_rx.is_some() && self.engine_tier == self.tier {
            return;
        }
        if self.engine.is_some() && self.engine_tier == self.tier && self.load_rx.is_none() {
            return;
        }
        let tier = self.tier;
        self.engine = None;
        self.loaded_engine = false;
        self.engine_tier = tier;
        self.engine_log.clear();
        let (tx, rx) = channel();
        self.load_rx = Some(rx);
        std::thread::spawn(move || {
            let cfg = EngineConfig {
                tier: tier.as_tier(),
                ..Default::default()
            };
            let _ = tx.send(LoadMsg::Progress(format!(
                "cargando motor ({})…",
                tier.label()
            )));
            match Engine::load(&cfg, None) {
                Ok(e) => {
                    let _ = tx.send(LoadMsg::Ready(Ok(e)));
                }
                Err(err) => {
                    let _ = tx.send(LoadMsg::Ready(Err(format!("{err:#}"))));
                }
            }
        });
    }

    /// Pollea el hilo de carga del motor y aplica lo que llegue.
    fn poll_engine(&mut self) {
        let Some(rx) = self.load_rx.take() else {
            return;
        };
        let mut keep = false;
        loop {
            match rx.try_recv() {
                Ok(LoadMsg::Progress(p)) => {
                    self.engine_log.push(p);
                    keep = true;
                }
                Ok(LoadMsg::Ready(Ok(e))) => {
                    self.engine = Some(e);
                    self.loaded_engine = true;
                    self.engine_log.push("motor listo".into());
                    self.status = format!(
                        "Motor {} listo. Abre una imagen y pulsa OCR.",
                        self.engine_tier.label()
                    );
                }
                Ok(LoadMsg::Ready(Err(e))) => {
                    self.status = format!("error cargando motor: {e}");
                    self.loaded_engine = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    keep = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if keep {
            self.load_rx = Some(rx);
        }
    }

    fn open_path(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        match image_io::load_images(path) {
            Ok(pages) => {
                self.path = Some(path.to_path_buf());
                self.pages = pages;
                self.page = 0;
                self.results = vec![Vec::new(); self.pages.len()];
                self.elapsed_ms = vec![0; self.pages.len()];
                self.selected = None;
                self.zoom = 1.0;
                self.pan = egui::Vec2::ZERO;
                self.orig_bytes = std::fs::read(path).ok();
                self.status = format!(
                    "{} ({}x{}) | {} páginas",
                    path.display(),
                    self.pages[0].width(),
                    self.pages[0].height(),
                    self.pages.len()
                );
                self.swap_page(ctx);
            }
            Err(e) => self.status = format!("error abriendo {}: {e:#}", path.display()),
        }
    }

    /// (Re)carga la textura de la página actual.
    fn swap_page(&mut self, ctx: &egui::Context) {
        let Some(img) = self.pages.get(self.page) else {
            self.texture = None;
            return;
        };
        let rgb = img.to_rgb8();
        let size = [rgb.width() as usize, rgb.height() as usize];
        let color = egui::ColorImage::from_rgb(size, &rgb);
        self.texture =
            Some(ctx.load_texture(format!("page-{}", self.page), color, Default::default()));
    }

    fn run_ocr(&mut self, ctx: &egui::Context) {
        let Some(engine) = &self.engine else {
            self.status = "el motor aún no está listo".into();
            return;
        };
        let Some(img) = self.pages.get(self.page) else {
            return;
        };
        let t0 = Instant::now();
        match engine.ocr(img) {
            Ok(lines) => {
                self.results[self.page] = lines;
                self.elapsed_ms[self.page] = t0.elapsed().as_millis();
                self.selected = None;
                self.status = format!(
                    "{} → {} líneas en {} ms (página {}/{})",
                    self.path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    self.results[self.page].len(),
                    self.elapsed_ms[self.page],
                    self.page + 1,
                    self.pages.len()
                );
            }
            Err(e) => self.status = format!("OCR falló: {e:#}"),
        }
        ctx.request_repaint();
    }

    fn page_lines(&self) -> &[OcrLine] {
        self.results.get(self.page).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn export_txt(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Texto", &["txt"])
            .set_file_name("octxt.txt")
            .save_file()
        else {
            return;
        };
        let mut body = String::new();
        for (i, lines) in self.results.iter().enumerate() {
            if self.pages.len() > 1 {
                body.push_str(&format!("# Página {}\n\n", i + 1));
            }
            body.push_str(&txt::plain(lines));
            body.push('\n');
        }
        if let Err(e) = std::fs::write(&path, body) {
            self.status = format!("error exportando TXT: {e}");
        } else {
            self.status = format!("✓ TXT → {}", path.display());
        }
    }

    fn export_json(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("octxt.json")
            .save_file()
        else {
            return;
        };
        let mut out = Vec::new();
        for (i, img) in self.pages.iter().enumerate() {
            let r = FileResult {
                file: format!(
                    "{}#p{}",
                    self.path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    i + 1
                ),
                width: img.width(),
                height: img.height(),
                elapsed_ms: self.elapsed_ms.get(i).copied().unwrap_or(0),
                lines: self.results[i].clone(),
            };
            out.push(json::FileJson::from_result(&r));
        }
        let body = match serde_json::to_string_pretty(&out) {
            Ok(b) => b,
            Err(e) => {
                self.status = format!("error serializando JSON: {e}");
                return;
            }
        };
        if let Err(e) = std::fs::write(&path, body) {
            self.status = format!("error exportando JSON: {e}");
        } else {
            self.status = format!("✓ JSON → {}", path.display());
        }
    }

    fn export_pdf(&mut self) {
        let Some(img) = self.pages.get(self.page) else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name("octxt.pdf")
            .save_file()
        else {
            return;
        };
        let bytes = if self.page == 0 {
            self.orig_bytes.as_deref()
        } else {
            None
        };
        let dated = self.results.get(self.page).map(|v| v.as_slice()).unwrap_or(&[]);
        if let Err(e) = pdf::searchable_pdf(bytes, img, dated, &path) {
            self.status = format!("error exportando PDF: {e:#}");
        } else {
            self.status = format!("✓ PDF → {}", path.display());
        }
    }
}

impl eframe::App for OcrtApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_engine();
        if self.load_rx.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        // --- Drag & drop / keyboard shortcuts ---------------------------------
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
        let hovered = ui.ctx().input(|i| i.raw.hovered_files.clone());
        self.drop_hover = !hovered.is_empty();
        if let Some(d) = dropped.last() {
            if let Some(p) = &d.path {
                self.open_path(ui.ctx(), p);
            }
        }
        if ui.ctx().input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            pick_and_open(self, ui.ctx());
        }
        if ui.ctx().input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            self.run_ocr(ui.ctx());
        }

        // --- Paneles ------------------------------------------------------------
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar_ui(ui));
        egui::Panel::right("results_panel")
            .default_size(360.0)
            .min_size(200.0)
            .show(ui, |ui| self.results_ui(ui));
        egui::Panel::bottom("statusbar").show(ui, |ui| self.statusbar_ui(ui));

        egui::CentralPanel::default().show(ui, |ui| self.image_ui(ui));
    }
}

// ---------------------------------------------------------------------------

impl OcrtApp {
    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("octxt");

            egui::ComboBox::from_id_salt("tier")
                .selected_text(self.tier.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.tier, GuiTier::Tiny, "tiny (rápido)");
                    ui.selectable_value(&mut self.tier, GuiTier::Small, "small");
                    ui.selectable_value(&mut self.tier, GuiTier::Medium, "medium (preciso)");
                });
            if self.tier != self.prev_tier {
                self.prev_tier = self.tier;
                self.request_engine_load();
                ui.ctx().request_repaint();
            }

            ui.separator();

            if ui.button("Abrir…").clicked() {
                pick_and_open(self, ui.ctx());
            }
            if ui
                .add_enabled(!self.pages.is_empty(), egui::Button::new("OCR (Ctrl+Enter)"))
                .clicked()
            {
                self.run_ocr(ui.ctx());
            }

            ui.separator();

            let has_results = self.page_lines().len() > 0;
            if ui
                .add_enabled(has_results && !self.pages.is_empty(), egui::Button::new("TXT"))
                .clicked()
            {
                self.export_txt();
            }
            if ui
                .add_enabled(has_results && !self.pages.is_empty(), egui::Button::new("JSON"))
                .clicked()
            {
                self.export_json();
            }
            if ui
                .add_enabled(has_results && !self.pages.is_empty(), egui::Button::new("PDF"))
                .clicked()
            {
                self.export_pdf();
            }

            ui.separator();

            // Selector de página.
            if self.pages.len() > 1 {
                ui.label("Página:");
                let mut p = self.page + 1;
                if ui
                    .add(egui::DragValue::new(&mut p).range(1..=self.pages.len()).speed(1.0))
                    .changed()
                {
                    self.page = (p - 1).min(self.pages.len() - 1);
                    self.selected = None;
                    self.swap_page(ui.ctx());
                }
                ui.label(format!("/ {}", self.pages.len()));
            }
        });
    }

    fn statusbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.drop_hover {
                ui.colored_label(egui::Color32::from_rgb(90, 190, 255), "⤓ suelta para abrir");
            }
            ui.label(&self.status);
            if self.load_rx.is_some() || (self.loaded_engine && self.engine.is_none()) {
                ui.spinner();
            }
        });
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Resultados");
        ui.separator();

        let lines: Vec<OcrLine> = self.page_lines().to_vec();
        if lines.is_empty() {
            ui.weak("Sin resultados todavía.");
            return;
        }

        ui.weak(format!("{} líneas · {} ms", lines.len(), self.elapsed_ms[self.page]));
        for (i, line) in lines.iter().enumerate() {
            let tb = format!("det {:.2} · rec {:.2}", line.det_score, line.rec_score);
            let text = if line.text.trim().is_empty() {
                "(vacío)".to_string()
            } else {
                line.text.clone()
            };
            let resp = ui
                .selectable_label(self.selected == Some(i), text)
                .on_hover_text(&tb);
            if resp.clicked() {
                self.selected = Some(i);
            }
            ui.weak(tb);
            ui.add_space(4.0);
        }
    }

    /// Visor de imagen con zoom/pane y overlay de cajas.
    fn image_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tex) = &self.texture else {
            ui.centered_and_justified(|ui| {
                ui.label("Ninguna imagen cargada.");
            });
            return;
        };

        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

        // Zoom con scroll y Ctrl+scroll; pan con arrastre.
        if resp.hovered() {
            let scroll = ui.ctx().input(|i| i.smooth_scroll_delta().y);
            if scroll != 0.0 {
                let factor = (scroll * 0.01).exp();
                self.zoom = (self.zoom * factor).clamp(0.05, 32.0);
            }
        }
        if resp.dragged() {
            let delta = ui.ctx().input(|i| i.pointer.delta());
            self.pan += delta;
        }

        let img_size = tex.size_vec2().abs();
        let base = (rect.width() / img_size.x).min(rect.height() / img_size.y);
        let scale = base * self.zoom;
        let drawn = egui::vec2(img_size.x * scale, img_size.y * scale);
        let tl = rect.center() - drawn / 2.0 + self.pan;
        let img_rect = egui::Rect::from_min_size(tl, drawn);

        let painter = ui.painter_at(rect);

        // Fondo y comprobación de click en la imagen.
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(18));
        painter.image(tex.id(), img_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);

        // Overlay: cajas detectadas.
        let lines = self.page_lines();
        for (i, line) in lines.iter().enumerate() {
            let screen: Vec<egui::Pos2> = line
                .points
                .iter()
                .map(|(x, y)| {
                    let p = egui::pos2(x * scale, y * scale);
                    egui::pos2(img_rect.min.x + p.x, img_rect.min.y + p.y)
                })
                .collect();
            if screen.len() < 4 {
                continue;
            }
            let selected = self.selected == Some(i);
            let color = if selected {
                egui::Color32::from_rgb(255, 180, 20)
            } else {
                egui::Color32::from_rgb(80, 200, 255)
            };
            let stroke = if selected {
                egui::Stroke::new(2.5, color)
            } else {
                egui::Stroke::new(1.0, color)
            };
            painter.add(egui::Shape::convex_polygon(screen.clone(), egui::Color32::TRANSPARENT, stroke));
            let _ = (i, selected);
        }

        // Click para seleccionar la caja más cercana.
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let lines = self.page_lines();
                let mut best: Option<(usize, f32)> = None;
                for (i, line) in lines.iter().enumerate() {
                    let cy = line.points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                    let cx = line.points.iter().map(|p| p.0).fold(f32::MAX, f32::min);
                    let ix = (pos.x - img_rect.min.x) / scale;
                    let iy = (pos.y - img_rect.min.y) / scale;
                    let dx = ix - cx;
                    let dy = iy - cy;
                    let d = dx * dx + dy * dy;
                    if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                        best = Some((i, d));
                    }
                }
                self.selected = best.map(|(i, _)| i);
            }
        }

        // Doble click: reset de zoom/pan.
        if resp.double_clicked() {
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
        }
    }
}

// ---------------------------------------------------------------------------

fn pick_and_open(app: &mut OcrtApp, ctx: &egui::Context) {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Imágenes", &["png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff"])
        .set_title("Abrir imagen")
        .pick_file()
    else {
        return;
    };
    app.open_path(ctx, &path);
}

/// Añade fuentes CJK del sistema si están disponibles (para glifos chinos).
fn setup_cjk_fonts(ctx: &egui::Context) {
    use std::sync::Arc;
    let mut defs = egui::FontDefinitions::default();

    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",      // Microsoft YaHei
        "C:/Windows/Fonts/simsun.ttc",    // SimSun
        "C:/Windows/Fonts/notosanscjk.ttc",
        "C:/Windows/Fonts/Deng.ttf",      // DengXian
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            defs.font_data.insert(
                "cjk".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            defs.families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push("cjk".to_owned());
            defs.families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("cjk".to_owned());
            break;
        }
    }
    ctx.set_fonts(defs);
}

// ---------------------------------------------------------------------------

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("octxt — OCR PP-OCRv6")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        APP_ID,
        options,
        Box::new(|cc| Ok(Box::new(OcrtApp::new(&cc.egui_ctx)))),
    )
}