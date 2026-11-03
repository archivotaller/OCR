//! octxt-gui: interfaz de escritorio del OCR (eframe/egui 0.35).
//!
//! - Abre imágenes, TIFF multipágina o carpetas completas (botón o arrastrando).
//! - Carga el motor y ejecuta el OCR en un hilo aparte: la interfaz no se bloquea.
//! - Procesa una página, un documento entero o todos los documentos en cola.
//! - Exporta TXT / JSON y PDF buscable de todas las páginas.

use octxt::image_io;
use octxt::models::Tier;
use octxt::ocr::engine::{Engine, EngineConfig, OcrLine};
use octxt::output::{common::FileResult, json, pdf, txt};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
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

/// Mensajes hacia el hilo del motor.
enum WorkerCmd {
    Load(GuiTier),
    Ocr {
        doc: usize,
        page: usize,
        image: image::DynamicImage,
    },
    Shutdown,
}

/// Mensajes desde el hilo del motor.
enum WorkerEvt {
    EngineLoading(GuiTier),
    EngineReady(Result<usize, String>),
    OcrDone {
        doc: usize,
        page: usize,
        lines: Vec<OcrLine>,
        elapsed_ms: u128,
    },
    OcrFailed {
        doc: usize,
        page: usize,
        error: String,
    },
}

/// Un documento abierto (una imagen suelta o un TIFF multipágina).
struct Doc {
    path: PathBuf,
    pages: Vec<image::DynamicImage>,
    results: Vec<Vec<OcrLine>>,
    elapsed_ms: Vec<u128>,
    orig_bytes: Option<Vec<u8>>,
}

impl Doc {
    fn from_path(path: &Path) -> anyhow::Result<Self> {
        let pages = image_io::load_images(path)?;
        Ok(Doc {
            path: path.to_path_buf(),
            results: vec![Vec::new(); pages.len()],
            elapsed_ms: vec![0; pages.len()],
            orig_bytes: std::fs::read(path).ok(),
            pages,
        })
    }

    fn line_count(&self) -> usize {
        self.results.iter().map(|l| l.len()).sum()
    }
}

struct TextureEntry {
    doc: usize,
    page: usize,
    handle: egui::TextureHandle,
}

#[derive(PartialEq, Eq)]
enum EngineState {
    Loading,
    Ready,
    Failed,
}

// ---------------------------------------------------------------------------

struct OcrtApp {
    tier: GuiTier,
    prev_tier: GuiTier,
    cmd_tx: Sender<WorkerCmd>,
    evt_rx: Receiver<WorkerEvt>,
    engine_state: EngineState,
    engine_threads: usize,

    docs: Vec<Doc>,
    doc: usize,
    page: usize,
    texture: Option<TextureEntry>,

    zoom: f32,
    pan: egui::Vec2,
    selected: Option<usize>,

    status: String,
    drop_hover: bool,

    /// Ejecutar OCR de todo lo abierto en cuanto el motor esté listo (`--ocr`).
    auto_ocr: bool,

    /// Páginas pendientes de OCR (doc, página).
    queue: VecDeque<(usize, usize)>,
    in_flight: Option<(usize, usize)>,
    /// Páginas del lote actual (para el resumen final).
    queued_total: usize,
}

impl OcrtApp {
    fn new(ctx: &egui::Context, tier: GuiTier, paths: Vec<PathBuf>, auto_ocr: bool) -> Self {
        setup_cjk_fonts(ctx);

        let (cmd_tx, evt_rx) = spawn_worker();
        let mut app = OcrtApp {
            tier,
            prev_tier: tier,
            cmd_tx,
            evt_rx,
            engine_state: EngineState::Loading,
            engine_threads: 0,
            docs: Vec::new(),
            doc: 0,
            page: 0,
            texture: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            selected: None,
            status: "Arrastra imágenes o usa 'Abrir…' / 'Carpeta…'.".into(),
            drop_hover: false,
            auto_ocr,
            queue: VecDeque::new(),
            in_flight: None,
            queued_total: 0,
        };
        app.request_engine_load();
        if !paths.is_empty() {
            app.open_paths(ctx, &paths);
        }
        app
    }

    fn request_engine_load(&mut self) {
        self.engine_state = EngineState::Loading;
        self.engine_threads = 0;

        let _ = self.cmd_tx.send(WorkerCmd::Load(self.tier));
    }

    // --- Documentos -------------------------------------------------------

    fn current_doc(&self) -> Option<&Doc> {
        self.docs.get(self.doc)
    }

    fn current_pages(&self) -> &[image::DynamicImage] {
        self.current_doc()
            .map(|d| d.pages.as_slice())
            .unwrap_or(&[])
    }

    fn current_lines(&self) -> &[OcrLine] {
        self.current_doc()
            .and_then(|d| d.results.get(self.page))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    fn total_pages(&self) -> usize {
        self.docs.iter().map(|d| d.pages.len()).sum()
    }

    fn open_paths(&mut self, ctx: &egui::Context, paths: &[PathBuf]) {
        let expanded = expand_paths(paths);
        if expanded.is_empty() {
            self.status = "no se encontraron imágenes en la selección".into();
            return;
        }

        let mut last: Option<usize> = None;
        let mut errors: Vec<String> = Vec::new();
        let mut opened = 0usize;
        for path in expanded {
            if let Some(i) = self.docs.iter().position(|d| d.path == path) {
                last = Some(i);
                continue;
            }
            match Doc::from_path(&path) {
                Ok(doc) => {
                    self.docs.push(doc);
                    last = Some(self.docs.len() - 1);
                    opened += 1;
                }
                Err(e) => errors.push(format!("{}: {e:#}", path.display())),
            }
        }

        if let Some(i) = last {
            self.doc = i;
            self.page = 0;
            self.selected = None;
            self.zoom = 1.0;
            self.pan = egui::Vec2::ZERO;
            self.texture = None;
        }

        let doc = self.current_doc();
        let pages = doc.map(|d| d.pages.len()).unwrap_or(0);
        let name = doc
            .map(|d| d.path.display().to_string())
            .unwrap_or_default();
        self.status = if errors.is_empty() {
            format!(
                "{name} | {pages} página(s) | {} documento(s), {} página(s) en total",
                self.docs.len(),
                self.total_pages()
            )
        } else {
            format!(
                "abiertos {opened}; {} error(es): {}",
                errors.len(),
                errors.join(" · ")
            )
        };
        ctx.request_repaint();
    }

    fn close_current(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        self.docs.remove(self.doc);
        self.queue.clear();
        self.in_flight = None;
        self.page = 0;
        self.selected = None;
        self.texture = None;
        self.doc = self.doc.min(self.docs.len().saturating_sub(1));
        self.status = if self.docs.is_empty() {
            "sin documentos abiertos".into()
        } else {
            format!("documento cerrado; quedan {}", self.docs.len())
        };
    }

    fn ensure_texture(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.docs.get(self.doc) else {
            self.texture = None;
            return;
        };
        let Some(img) = doc.pages.get(self.page) else {
            self.texture = None;
            return;
        };
        if let Some(t) = &self.texture {
            if t.doc == self.doc && t.page == self.page {
                return;
            }
        }
        let rgb = img.to_rgb8();
        let size = [rgb.width() as usize, rgb.height() as usize];
        let color = egui::ColorImage::from_rgb(size, &rgb);
        let handle = ctx.load_texture(
            format!("page-{}-{}", self.doc, self.page),
            color,
            Default::default(),
        );
        self.texture = Some(TextureEntry {
            doc: self.doc,
            page: self.page,
            handle,
        });
    }

    // --- Cola de OCR ------------------------------------------------------

    fn enqueue(&mut self, doc: usize, page: usize) {
        if self.in_flight == Some((doc, page)) || self.queue.contains(&(doc, page)) {
            return;
        }
        let Some(d) = self.docs.get(doc) else {
            return;
        };
        if page >= d.pages.len() {
            return;
        }
        self.queue.push_back((doc, page));
    }

    fn enqueue_current(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        if self.engine_state != EngineState::Ready {
            self.status = "el motor aún no está listo".into();
            return;
        }
        self.enqueue(self.doc, self.page);
        self.queued_total = self.queue.len() + usize::from(self.in_flight.is_some());
        self.status = format!("en cola: {} página(s)", self.queue.len());
    }

    fn enqueue_doc(&mut self) {
        let Some(d) = self.docs.get(self.doc) else {
            return;
        };
        if self.engine_state != EngineState::Ready {
            self.status = "el motor aún no está listo".into();
            return;
        }
        for page in 0..d.pages.len() {
            self.enqueue(self.doc, page);
        }
        self.queued_total = self.queue.len() + usize::from(self.in_flight.is_some());
        self.status = format!(
            "en cola: {} página(s) del documento actual",
            self.queue.len()
        );
    }

    fn enqueue_all(&mut self) {
        if self.engine_state != EngineState::Ready {
            self.status = "el motor aún no está listo".into();
            return;
        }
        for doc in 0..self.docs.len() {
            let pages = self.docs[doc].pages.len();
            for page in 0..pages {
                self.enqueue(doc, page);
            }
        }
        self.queued_total = self.queue.len() + usize::from(self.in_flight.is_some());
        self.status = format!(
            "en cola: {} página(s) de todos los documentos",
            self.queue.len()
        );
    }

    fn pump_queue(&mut self, ctx: &egui::Context) {
        if self.in_flight.is_some() {
            return;
        }
        let Some((doc, page)) = self.queue.pop_front() else {
            return;
        };
        let Some(image) = self.docs.get(doc).and_then(|d| d.pages.get(page)).cloned() else {
            return;
        };
        if self
            .cmd_tx
            .send(WorkerCmd::Ocr { doc, page, image })
            .is_ok()
        {
            self.in_flight = Some((doc, page));
            self.status = format!(
                "OCR {} · página {}/{}… ({} en cola)",
                self.docs
                    .get(doc)
                    .map(|d| d
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned())
                    .unwrap_or_default(),
                page + 1,
                self.docs.get(doc).map(|d| d.pages.len()).unwrap_or(0),
                self.queue.len()
            );
            ctx.request_repaint();
        }
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let mut keep_going = false;
        loop {
            match self.evt_rx.try_recv() {
                Ok(WorkerEvt::EngineLoading(tier)) => {
                    self.engine_state = EngineState::Loading;
                    self.status = format!("cargando motor {}…", tier.label());
                    keep_going = true;
                }
                Ok(WorkerEvt::EngineReady(Ok(threads))) => {
                    self.engine_state = EngineState::Ready;
                    self.engine_threads = threads;
                    self.status = format!(
                        "motor listo ({} hilos). Abre una imagen y pulsa OCR.",
                        threads
                    );
                    if self.auto_ocr {
                        self.auto_ocr = false;
                        if !self.docs.is_empty() {
                            self.enqueue_all();
                        }
                    }
                }
                Ok(WorkerEvt::EngineReady(Err(e))) => {
                    self.engine_state = EngineState::Failed;

                    self.status = format!("error cargando el motor: {e}");
                }
                Ok(WorkerEvt::OcrDone {
                    doc,
                    page,
                    lines,
                    elapsed_ms,
                }) => {
                    if let Some(d) = self.docs.get_mut(doc) {
                        if page < d.results.len() {
                            d.results[page] = lines;
                            d.elapsed_ms[page] = elapsed_ms;
                        }
                    }
                    if self.in_flight == Some((doc, page)) {
                        self.in_flight = None;
                    }
                    let remaining = self.queue.len();
                    if remaining == 0 {
                        let lines = self.current_doc().map(Doc::line_count).unwrap_or(0);
                        let processed = self.queued_total.max(1);
                        self.status = format!(
                            "OCR completado: {processed} página(s), {lines} línea(s) en el documento actual"
                        );
                        self.queued_total = 0;
                    } else {
                        self.status = format!("OCR listo; quedan {remaining} página(s)");
                    }
                }
                Ok(WorkerEvt::OcrFailed { doc, page, error }) => {
                    if self.in_flight == Some((doc, page)) {
                        self.in_flight = None;
                    }
                    self.status = format!(
                        "OCR falló en {} p{}: {error}",
                        self.docs
                            .get(doc)
                            .map(|d| d.path.display().to_string())
                            .unwrap_or_default(),
                        page + 1
                    );
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.engine_state = EngineState::Failed;

                    break;
                }
            }
        }
        if keep_going
            || self.engine_state == EngineState::Loading
            || self.in_flight.is_some()
            || !self.queue.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    // --- Exportación ------------------------------------------------------

    fn file_result(&self, doc: usize, page: usize) -> Option<FileResult> {
        let d = self.docs.get(doc)?;
        let img = d.pages.get(page)?;
        let lines = d.results.get(page)?;
        let file = if d.pages.len() > 1 {
            format!("{}#p{}", d.path.display(), page + 1)
        } else {
            d.path.display().to_string()
        };
        Some(FileResult {
            file,
            width: img.width(),
            height: img.height(),
            elapsed_ms: d.elapsed_ms.get(page).copied().unwrap_or(0),
            lines: lines.clone(),
        })
    }

    fn export_txt(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Texto", &["txt"])
            .set_file_name("octxt.txt")
            .save_file()
        else {
            return;
        };
        let mut body = String::new();
        for d in &self.docs {
            for (page, lines) in d.results.iter().enumerate() {
                if self.docs.len() > 1 || d.pages.len() > 1 {
                    body.push_str(&format!(
                        "# {} — página {}/{}\n\n",
                        d.path.display(),
                        page + 1,
                        d.pages.len()
                    ));
                }
                body.push_str(&txt::plain(lines));
                body.push('\n');
            }
        }
        if let Err(e) = std::fs::write(&path, body) {
            self.status = format!("error exportando TXT: {e}");
        } else {
            self.status = format!("✓ TXT → {}", path.display());
        }
    }

    fn export_json(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .set_file_name("octxt.json")
            .save_file()
        else {
            return;
        };
        let mut out = Vec::new();
        for doc in 0..self.docs.len() {
            for page in 0..self.docs[doc].pages.len() {
                if let Some(r) = self.file_result(doc, page) {
                    out.push(json::FileJson::from_result(&r));
                }
            }
        }
        match serde_json::to_string_pretty(&out) {
            Ok(body) => {
                if let Err(e) = std::fs::write(&path, body) {
                    self.status = format!("error exportando JSON: {e}");
                } else {
                    self.status = format!("✓ JSON → {}", path.display());
                }
            }
            Err(e) => self.status = format!("error serializando JSON: {e}"),
        }
    }

    /// PDF buscable con **todas las páginas** del documento actual. Si hay
    /// varios documentos abiertos, escribe un PDF por documento en una carpeta.
    fn export_pdf(&mut self) {
        if self.docs.is_empty() {
            return;
        }
        let has_results = self
            .docs
            .iter()
            .any(|d| d.results.iter().any(|l| !l.is_empty()));
        if !has_results {
            self.status = "no hay resultados OCR para exportar a PDF".into();
            return;
        }

        if self.docs.len() == 1 {
            let Some(path) = rfd::FileDialog::new()
                .add_filter("PDF", &["pdf"])
                .set_file_name("octxt.pdf")
                .save_file()
            else {
                return;
            };
            match write_doc_pdf(&self.docs[0], &path) {
                Ok(()) => self.status = format!("✓ PDF → {}", path.display()),
                Err(e) => self.status = format!("error exportando PDF: {e:#}"),
            }
            return;
        }

        let Some(dir) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        let mut written = 0usize;
        let mut failures: Vec<String> = Vec::new();
        for d in &self.docs {
            let stem = d
                .path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "documento".into());
            let out = dir.join(format!("{stem}.pdf"));
            match write_doc_pdf(d, &out) {
                Ok(()) => written += 1,
                Err(e) => failures.push(format!("{}: {e:#}", d.path.display())),
            }
        }
        self.status = if failures.is_empty() {
            format!("✓ {written} PDF(s) en {}", dir.display())
        } else {
            format!(
                "escritos {written} PDF(s); {} error(es): {}",
                failures.len(),
                failures.join(" · ")
            )
        };
    }
}

impl Drop for OcrtApp {
    fn drop(&mut self) {
        // El hilo del motor termina al recibir Shutdown (o al cerrarse el canal).
        let _ = self.cmd_tx.send(WorkerCmd::Shutdown);
    }
}

/// Escribe un PDF con todas las páginas de `doc`.
fn write_doc_pdf(doc: &Doc, out: &Path) -> anyhow::Result<()> {
    let pages: Vec<pdf::PdfPage<'_>> = doc
        .pages
        .iter()
        .enumerate()
        .map(|(i, img)| pdf::PdfPage {
            orig_bytes: if i == 0 {
                doc.orig_bytes.as_deref()
            } else {
                None
            },
            image: img,
            lines: &doc.results[i],
        })
        .collect();
    pdf::searchable_pdf_pages(&pages, out)
}

impl eframe::App for OcrtApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_events(ui.ctx());
        self.pump_queue(ui.ctx());
        self.ensure_texture(ui.ctx());

        // --- Drag & drop / atajos -----------------------------------------
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.clone());
        let hovered = ui.ctx().input(|i| i.raw.hovered_files.clone());
        self.drop_hover = !hovered.is_empty();
        if !dropped.is_empty() {
            let paths: Vec<PathBuf> = dropped.iter().filter_map(|d| d.path.clone()).collect();
            if !paths.is_empty() {
                self.open_paths(ui.ctx(), &paths);
            }
        }
        if ui
            .ctx()
            .input(|i| i.modifiers.command && i.key_pressed(egui::Key::O))
        {
            pick_and_open(self, ui.ctx());
        }
        if ui
            .ctx()
            .input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter))
        {
            if ui.ctx().input(|i| i.modifiers.shift) {
                self.enqueue_all();
            } else {
                self.enqueue_current();
            }
        }

        // --- Paneles -------------------------------------------------------
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar_ui(ui));
        egui::Panel::right("results_panel")
            .default_size(380.0)
            .min_size(220.0)
            .show(ui, |ui| self.results_ui(ui));
        egui::Panel::bottom("statusbar").show(ui, |ui| self.statusbar_ui(ui));
        egui::CentralPanel::default().show(ui, |ui| self.image_ui(ui));
    }
}

// ---------------------------------------------------------------------------

impl OcrtApp {
    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
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
            if ui.button("Carpeta…").clicked() {
                pick_folder(self, ui.ctx());
            }
            if ui
                .add_enabled(!self.docs.is_empty(), egui::Button::new("Cerrar doc"))
                .clicked()
            {
                self.close_current();
            }

            ui.separator();

            let ready = self.engine_state == EngineState::Ready;
            if ui
                .add_enabled(
                    ready && !self.docs.is_empty(),
                    egui::Button::new("OCR página (Ctrl+Enter)"),
                )
                .clicked()
            {
                self.enqueue_current();
            }
            if ui
                .add_enabled(ready && !self.docs.is_empty(), egui::Button::new("OCR doc"))
                .clicked()
            {
                self.enqueue_doc();
            }
            if ui
                .add_enabled(
                    ready && !self.docs.is_empty(),
                    egui::Button::new("OCR todo (Ctrl+Shift+Enter)"),
                )
                .clicked()
            {
                self.enqueue_all();
            }
            if ui
                .add_enabled(!self.queue.is_empty(), egui::Button::new("Vaciar cola"))
                .clicked()
            {
                self.queue.clear();
                self.status = "cola vaciada".into();
            }

            ui.separator();

            let has_results = self.docs.iter().any(|d| d.line_count() > 0);
            let can_export = has_results && !self.docs.is_empty();
            if ui
                .add_enabled(can_export, egui::Button::new("TXT"))
                .clicked()
            {
                self.export_txt();
            }
            if ui
                .add_enabled(can_export, egui::Button::new("JSON"))
                .clicked()
            {
                self.export_json();
            }
            if ui
                .add_enabled(can_export, egui::Button::new("PDF"))
                .clicked()
            {
                self.export_pdf();
            }

            ui.separator();

            // Selector de documento.
            if self.docs.len() > 1 {
                let current = self
                    .docs
                    .get(self.doc)
                    .and_then(|d| d.path.file_name())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                egui::ComboBox::from_id_salt("doc")
                    .selected_text(format!("{current} ({}/{})", self.doc + 1, self.docs.len()))
                    .show_ui(ui, |ui| {
                        for (i, d) in self.docs.iter().enumerate() {
                            let name = d
                                .path
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.selectable_value(&mut self.doc, i, name);
                        }
                    });
                if self.docs.get(self.doc).map(|d| d.pages.len()).unwrap_or(0) > 1
                    || self.page >= self.current_pages().len()
                {
                    self.page = self.page.min(self.current_pages().len().saturating_sub(1));
                }
            }

            // Selector de página.
            let pages = self.current_pages().len();
            if pages > 1 {
                ui.label("Página:");
                let mut p = self.page + 1;
                if ui
                    .add(egui::DragValue::new(&mut p).range(1..=pages).speed(1.0))
                    .changed()
                {
                    self.page = (p - 1).min(pages - 1);
                    self.selected = None;
                }
                ui.label(format!("/ {pages}"));
            }
        });
    }

    fn statusbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if self.drop_hover {
                ui.colored_label(egui::Color32::from_rgb(90, 190, 255), "⤓ suelta para abrir");
            }
            ui.label(&self.status);
            if self.engine_state == EngineState::Loading
                || self.in_flight.is_some()
                || !self.queue.is_empty()
            {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.docs.get(self.doc).map(|d| d.pages.len()).unwrap_or(0) > 1 {
                    ui.label(format!(
                        "página {}/{}",
                        self.page + 1,
                        self.current_pages().len()
                    ));
                }
                if !self.docs.is_empty() {
                    ui.label(format!(
                        "{} doc · {} pág",
                        self.docs.len(),
                        self.total_pages()
                    ));
                }
                if self.engine_state == EngineState::Ready {
                    ui.weak(format!("{} hilos", self.engine_threads));
                }
            });
        });
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Resultados");
            if self.in_flight.is_some() || !self.queue.is_empty() {
                ui.spinner();
            }
        });
        ui.separator();

        // Copia local: el closure de la lista necesita `&mut self` para la
        // selección, así que no se puede mantener prestado el documento.
        let lines = self.current_lines().to_vec();
        if lines.is_empty() {
            ui.weak("Sin resultados todavía para esta página.");
            return;
        }

        let elapsed = self
            .current_doc()
            .and_then(|d| d.elapsed_ms.get(self.page))
            .copied()
            .unwrap_or(0);
        ui.weak(format!("{} líneas · {elapsed} ms", lines.len()));

        egui::ScrollArea::vertical().show(ui, |ui| {
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
        });
    }

    /// Visor de imagen con zoom/pan y overlay de cajas.
    fn image_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tex) = &self.texture else {
            ui.centered_and_justified(|ui| {
                ui.label("Ninguna imagen cargada.");
            });
            return;
        };

        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());

        // Zoom con scroll y pan con arrastre.
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

        let img_size = tex.handle.size_vec2().abs();
        let base = (rect.width() / img_size.x).min(rect.height() / img_size.y);
        let scale = base * self.zoom;
        let drawn = egui::vec2(img_size.x * scale, img_size.y * scale);
        let tl = rect.center() - drawn / 2.0 + self.pan;
        let img_rect = egui::Rect::from_min_size(tl, drawn);

        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, egui::Color32::from_gray(18));
        painter.image(
            tex.handle.id(),
            img_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );

        // Overlay: cajas detectadas.
        let lines = self.current_lines();
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
            painter.add(egui::Shape::convex_polygon(
                screen,
                egui::Color32::TRANSPARENT,
                stroke,
            ));
        }

        // Click para seleccionar la caja más cercana.
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let mut best: Option<(usize, f32)> = None;
                for (i, line) in self.current_lines().iter().enumerate() {
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

/// Hilo dedicado: carga el motor una vez y procesa trabajos de OCR en serie.
fn spawn_worker() -> (Sender<WorkerCmd>, Receiver<WorkerEvt>) {
    let (cmd_tx, cmd_rx) = channel::<WorkerCmd>();
    let (evt_tx, evt_rx) = channel::<WorkerEvt>();
    std::thread::spawn(move || {
        let mut engine: Option<Engine> = None;
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                WorkerCmd::Load(tier) => {
                    engine = None;
                    let _ = evt_tx.send(WorkerEvt::EngineLoading(tier));
                    let cfg = EngineConfig {
                        tier: tier.as_tier(),
                        ..Default::default()
                    };
                    match Engine::load(&cfg, None) {
                        Ok(e) => {
                            let threads = e.pool_threads();
                            engine = Some(e);
                            let _ = evt_tx.send(WorkerEvt::EngineReady(Ok(threads)));
                        }
                        Err(e) => {
                            let _ = evt_tx.send(WorkerEvt::EngineReady(Err(format!("{e:#}"))));
                        }
                    }
                }
                WorkerCmd::Ocr { doc, page, image } => {
                    let t0 = Instant::now();
                    match engine.as_ref() {
                        Some(e) => match e.ocr(&image) {
                            Ok(lines) => {
                                let _ = evt_tx.send(WorkerEvt::OcrDone {
                                    doc,
                                    page,
                                    lines,
                                    elapsed_ms: t0.elapsed().as_millis(),
                                });
                            }
                            Err(e) => {
                                let _ = evt_tx.send(WorkerEvt::OcrFailed {
                                    doc,
                                    page,
                                    error: format!("{e:#}"),
                                });
                            }
                        },
                        None => {
                            let _ = evt_tx.send(WorkerEvt::OcrFailed {
                                doc,
                                page,
                                error: "el motor aún no está listo".into(),
                            });
                        }
                    }
                }
                WorkerCmd::Shutdown => break,
            }
        }
    });
    (cmd_tx, evt_rx)
}

/// Expande rutas (archivos, carpetas) a una lista de imágenes, sin duplicados.
fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            collect_dir(p, &mut out);
        } else if p.is_file() && image_io::has_image_ext(p) {
            out.push(p.clone());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn collect_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            collect_dir(&path, out);
        } else if ft.is_file() && image_io::has_image_ext(&path) {
            out.push(path);
        }
    }
}

fn pick_and_open(app: &mut OcrtApp, ctx: &egui::Context) {
    let Some(paths) = rfd::FileDialog::new()
        .add_filter(
            "Imágenes",
            &[
                "png", "jpg", "jpeg", "bmp", "gif", "webp", "tif", "tiff", "pnm",
            ],
        )
        .set_title("Abrir imágenes")
        .pick_files()
    else {
        return;
    };
    app.open_paths(ctx, &paths);
}

fn pick_folder(app: &mut OcrtApp, ctx: &egui::Context) {
    let Some(dir) = rfd::FileDialog::new()
        .set_title("Abrir carpeta")
        .pick_folder()
    else {
        return;
    };
    app.open_paths(ctx, &[dir]);
}

/// Añade fuentes CJK del sistema si están disponibles (para glifos chinos).
fn setup_cjk_fonts(ctx: &egui::Context) {
    use std::sync::Arc;
    let mut defs = egui::FontDefinitions::default();

    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",   // Microsoft YaHei
        "C:/Windows/Fonts/simsun.ttc", // SimSun
        "C:/Windows/Fonts/notosanscjk.ttc",
        "C:/Windows/Fonts/Deng.ttf", // DengXian
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

fn parse_tier(s: &str) -> Option<GuiTier> {
    match s.to_lowercase().as_str() {
        "tiny" => Some(GuiTier::Tiny),
        "small" => Some(GuiTier::Small),
        "medium" => Some(GuiTier::Medium),
        _ => None,
    }
}

fn main() -> eframe::Result {
    let mut tier = GuiTier::Medium;
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut auto_ocr = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--tier" => {
                if let Some(v) = args.next() {
                    match parse_tier(&v) {
                        Some(t) => tier = t,
                        None => {
                            eprintln!("tier desconocido '{v}' (tiny|small|medium)");
                            std::process::exit(2);
                        }
                    }
                }
            }
            "--ocr" => auto_ocr = true,
            "-h" | "--help" => {
                println!(
                    "octxt-gui [--tier tiny|small|medium] [--ocr] [imágenes o carpetas…]\n\n\
                     --ocr  ejecuta el OCR de todo lo abierto en cuanto carga el motor"
                );
                return Ok(());
            }
            other => paths.push(PathBuf::from(other)),
        }
    }

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
        Box::new(move |cc| Ok(Box::new(OcrtApp::new(&cc.egui_ctx, tier, paths, auto_ocr)))),
    )
}
