#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

mod diagnostics;
mod duplicates;
mod index;
mod ocr;
mod settings;
mod tray;
mod vision;
mod worker;

use eframe::egui::{self, Color32, ColorImage, RichText, Stroke, TextureHandle, TextureOptions};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use image::{DynamicImage, RgbaImage};
use settings::{CaptureRect, Settings};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use worker::{CaptureState, WorkerCommand, WorkerEvent};

const APP_TITLE: &str = "HDV Captures";
const HOTKEYS: &[&str] = &[
    "Ctrl + Maj + S",
    "Ctrl + Maj + D",
    "Ctrl + Alt + C",
    "F8",
    "F9",
];

// Charte graphique centralisee : aucune logique de capture ne depend de l'interface.
const BACKGROUND: Color32 = Color32::from_rgb(11, 19, 32);
const SURFACE: Color32 = Color32::from_rgb(20, 31, 48);
const SURFACE_ALT: Color32 = Color32::from_rgb(25, 39, 59);
const STROKE: Color32 = Color32::from_rgb(43, 63, 84);
const TEXT: Color32 = Color32::from_rgb(234, 244, 250);
const MUTED: Color32 = Color32::from_rgb(153, 174, 195);
const CYAN: Color32 = Color32::from_rgb(39, 209, 216);
const GREEN: Color32 = Color32::from_rgb(72, 225, 152);
const AMBER: Color32 = Color32::from_rgb(255, 181, 92);
const BLUE: Color32 = Color32::from_rgb(102, 174, 246);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Collecte,
    Reprendre,
    Captures,
    Parametres,
}

fn section_card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, STROKE))
        .inner_margin(egui::Margin::same(13))
        .corner_radius(egui::CornerRadius::same(9))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}

fn title(ui: &mut egui::Ui, name: &str, caption: &str) {
    ui.label(RichText::new(name).size(24.0).strong().color(TEXT));
    ui.label(RichText::new(caption).size(13.0).color(MUTED));
    ui.add_space(12.0);
}

fn metric(ui: &mut egui::Ui, value: String, label: &str, color: Color32) {
    section_card(ui, |ui| {
        ui.set_min_height(58.0);
        ui.horizontal(|ui| {
            let kind = if label.starts_with("Fiches") {
                4
            } else if label.starts_with("Enregistr") {
                5
            } else if label.starts_with("À reprendre") {
                1
            } else if label.starts_with("En attente") {
                6
            } else if label.starts_with("Analyse") {
                7
            } else {
                8
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(35.0, 35.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 19.0, color.gamma_multiply(0.08));
            draw_icon(
                ui.painter(),
                rect.min + egui::vec2(3.0, 3.0),
                29.0,
                kind,
                color,
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(value).size(23.0).strong().color(color));
                ui.label(RichText::new(label).size(11.0).color(MUTED));
            });
        });
    });
}

fn switch(ui: &mut egui::Ui, enabled: bool, color: Color32, label: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(94.0, 34.0), egui::Sense::click());
    let track = egui::Rect::from_min_size(rect.min, egui::vec2(58.0, 32.0));
    let painter = ui.painter();
    painter.rect_filled(
        track,
        16.0,
        if enabled {
            color.gamma_multiply(0.8)
        } else {
            STROKE
        },
    );
    painter.circle_filled(
        egui::pos2(
            if enabled {
                track.right() - 16.0
            } else {
                track.left() + 16.0
            },
            track.center().y,
        ),
        12.0,
        TEXT,
    );
    painter.text(
        egui::pos2(track.right() + 7.0, track.center().y),
        egui::Align2::LEFT_CENTER,
        if enabled { "ON" } else { "OFF" },
        egui::FontId::proportional(13.0),
        if enabled { color } else { MUTED },
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), enabled, label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn draw_icon(painter: &egui::Painter, origin: egui::Pos2, size: f32, kind: u8, color: Color32) {
    let point = |x: f32, y: f32| origin + egui::vec2(x * size, y * size);
    let stroke = Stroke::new(2.2_f32, color);
    match kind {
        0 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.06, 0.23), point(0.94, 0.87)),
                size * 0.1,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_stroke(point(0.5, 0.55), size * 0.19, stroke);
            painter.line_segment([point(0.28, 0.23), point(0.35, 0.1)], stroke);
            painter.line_segment([point(0.35, 0.1), point(0.65, 0.1)], stroke);
            painter.line_segment([point(0.65, 0.1), point(0.72, 0.23)], stroke);
        }
        1 => {
            painter.add(egui::Shape::closed_line(
                vec![point(0.5, 0.05), point(0.96, 0.91), point(0.04, 0.91)],
                stroke,
            ));
            painter.line_segment([point(0.5, 0.33), point(0.5, 0.59)], stroke);
            painter.circle_filled(point(0.5, 0.75), size * 0.035, color);
        }
        2 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.07, 0.07), point(0.93, 0.93)),
                size * 0.12,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_stroke(point(0.31, 0.31), size * 0.07, stroke);
            painter.add(egui::Shape::line(
                vec![
                    point(0.12, 0.76),
                    point(0.44, 0.45),
                    point(0.65, 0.65),
                    point(0.78, 0.5),
                    point(0.91, 0.64),
                ],
                stroke,
            ));
        }
        4 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.2, 0.05), point(0.8, 0.95)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            for y in [0.38, 0.56, 0.74] {
                painter.line_segment([point(0.34, y), point(0.68, y)], stroke);
            }
        }
        5 => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.41, stroke);
            painter.add(egui::Shape::line(
                vec![point(0.25, 0.5), point(0.44, 0.7), point(0.77, 0.29)],
                stroke,
            ));
        }
        6 => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.41, stroke);
            painter.add(egui::Shape::line(
                vec![point(0.5, 0.23), point(0.5, 0.51), point(0.7, 0.62)],
                stroke,
            ));
        }
        7 => {
            for n in 0..4 {
                let x = 0.12 + n as f32 * 0.22;
                let y = 0.67 - n as f32 * 0.17;
                painter.rect_filled(
                    egui::Rect::from_min_max(point(x, y), point(x + 0.12, 0.9)),
                    2.0,
                    color,
                );
            }
        }
        8 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.08, 0.2), point(0.92, 0.92)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([point(0.08, 0.4), point(0.92, 0.4)], stroke);
            for x in [0.3, 0.7] {
                painter.line_segment([point(x, 0.08), point(x, 0.3)], stroke);
            }
            for x in [0.3, 0.5, 0.7] {
                painter.circle_filled(point(x, 0.64), size * 0.04, color);
            }
        }
        _ => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.29, stroke);
            painter.circle_stroke(point(0.5, 0.5), size * 0.11, stroke);
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(a.cos(), a.sin());
                painter.line_segment(
                    [
                        point(0.5, 0.5) + dir * size * 0.29,
                        point(0.5, 0.5) + dir * size * 0.43,
                    ],
                    stroke,
                );
            }
        }
    }
}

fn shortcut(label: &str) -> HotKey {
    match label {
        "Ctrl + Maj + D" => HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD),
        "Ctrl + Alt + C" => HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyC),
        "F8" => HotKey::new(None, Code::F8),
        "F9" => HotKey::new(None, Code::F9),
        _ => HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyS),
    }
}

fn picture(
    ctx: &egui::Context,
    name: String,
    img: DynamicImage,
    max_w: u32,
    max_h: u32,
) -> TextureHandle {
    let image = img.thumbnail(max_w, max_h).to_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = image.into_raw();
    ctx.load_texture(
        name,
        ColorImage::from_rgba_unmultiplied(size, &pixels),
        TextureOptions::LINEAR,
    )
}

fn frame_texture(ctx: &egui::Context, name: &str, img: &RgbaImage) -> TextureHandle {
    let size = [img.width() as usize, img.height() as usize];
    ctx.load_texture(
        name,
        ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
        TextureOptions::LINEAR,
    )
}

// Le scan des fichiers et les lectures SQLite ne sont jamais effectues
// depuis le fil graphique (surtout apres une rafale de captures).
// Toutes les mutations de l'historique passent par une seule file FIFO.
// Une suppression ne peut donc pas être annulée par une écriture SQLite déjà en attente.
enum VisitWrite {
    Store(PathBuf, index::VisitRecord),
    Delete(PathBuf, i64, Sender<Result<Vec<i64>, String>>),
    ClearMissing(PathBuf, Sender<Result<Vec<i64>, String>>),
    ClearAll(PathBuf, Sender<Result<Vec<i64>, String>>),
}

#[derive(Clone)]
enum HistoryMutation {
    Delete(i64),
    ClearMissing,
    ClearAll,
}

struct GallerySnapshot {
    folder: PathBuf,
    count: Option<usize>,
    gallery_enabled: bool,
    files: Vec<PathBuf>,
    stats: index::Stats,
    prepared: HashSet<PathBuf>,
}

type HistoryReply = Receiver<Result<Vec<i64>, String>>;

struct App {
    #[cfg(windows)]
    tray: Option<tray::Tray>,
    settings: Settings,
    settings_path: PathBuf,
    cmd_tx: Sender<WorkerCommand>,
    event_rx: Receiver<WorkerEvent>,
    shortcut_manager: Option<GlobalHotKeyManager>,
    registered_hotkey: HotKey,
    monitors: Vec<String>,
    notice: String,
    notice_is_error: bool,
    capture_state: CaptureState,
    gallery: Vec<PathBuf>,
    capture_count: Option<usize>,
    gallery_scan_rx: Option<Receiver<GallerySnapshot>>,
    gallery_dirty: bool,
    gallery_changed_at: Instant,
    thumbs: HashMap<PathBuf, TextureHandle>,
    selected_visit: Option<index::VisitRecord>,
    selected_path: Option<PathBuf>,
    selected_texture: Option<TextureHandle>,
    calibration_image: Option<RgbaImage>,
    calibration_texture: Option<TextureHandle>,
    calibration_start: Option<egui::Pos2>,
    calibration_end: Option<egui::Pos2>,
    show_calibration: bool,
    show_help: bool,
    total_saved_session: usize,
    measured_visits: HashSet<i64>,
    analysis_seconds: f64,
    last_capture: Option<chrono::DateTime<chrono::Local>>,
    last_manual_hotkey: Instant,
    last_note: String,
    search: String,
    gallery_filter: u8,
    prepared_paths: HashSet<PathBuf>,
    stats: index::Stats,
    ignored: usize,
    rejected: usize,
    export_rx: Option<Receiver<Result<index::ExportResult, String>>>,
    show_diagnostic: bool,
    diagnostic_text: String,
    ocr_status: String,
    page: Page,
    tracking_only_missing: bool,
    visits: Vec<index::VisitRecord>,
    session_id: String,
    visit_tx: Sender<VisitWrite>,
    history_rx: Option<(HistoryMutation, HistoryReply)>,
    confirm_delete_visit: Option<i64>,
    confirm_clear_history: Option<bool>,
    // Une réponse OCR retardée ne doit pas recréer un suivi déjà effacé.
    dismissed_visit_ids: HashSet<i64>,
}

fn run_visit_writer(visit_rx: Receiver<VisitWrite>) {
    let mut dismissed: HashMap<PathBuf, HashSet<i64>> = HashMap::new();
    while let Ok(cmd) = visit_rx.recv() {
        match cmd {
            VisitWrite::Store(folder, record) => {
                if !dismissed
                    .get(&folder)
                    .is_some_and(|ids| ids.contains(&record.id))
                {
                    if let Err(e) = index::store_visit(&folder, &record) {
                        diagnostics::write("SUIVI", &e);
                    }
                }
            }
            VisitWrite::Delete(folder, id, ack) => {
                let result = index::delete_visit(&folder, id);
                if result.is_ok() {
                    dismissed.entry(folder).or_default().insert(id);
                }
                let _ = ack.send(result.map(|_| vec![id]));
            }
            VisitWrite::ClearMissing(folder, ack) => {
                let result = index::clear_visits_with_ids(&folder, true);
                let result = result.inspect(|ids| {
                    dismissed
                        .entry(folder)
                        .or_default()
                        .extend(ids.iter().copied());
                });
                let _ = ack.send(result);
            }
            VisitWrite::ClearAll(folder, ack) => {
                let result = index::clear_visits_with_ids(&folder, false);
                let result = result.inspect(|ids| {
                    dismissed
                        .entry(folder)
                        .or_default()
                        .extend(ids.iter().copied());
                });
                let _ = ack.send(result);
            }
        }
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let style = cc.egui_ctx.style();
        let mut style = (*style).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 9.0);
        style.spacing.window_margin = egui::Margin::same(14);
        style.spacing.button_padding = egui::vec2(11.0, 6.0);
        style.visuals.panel_fill = BACKGROUND;
        style.visuals.window_fill = SURFACE;
        style.visuals.selection.bg_fill = Color32::from_rgb(10, 113, 119);
        style.visuals.widgets.inactive.weak_bg_fill = SURFACE_ALT;
        style.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(41, 66, 91);
        style.visuals.widgets.active.weak_bg_fill = Color32::from_rgb(12, 126, 131);
        style.visuals.window_stroke = Stroke::new(1.0_f32, STROKE);
        cc.egui_ctx.set_style(style);

        let path = settings::config_path();
        let mut settings = settings::load(&path);
        // Démarrage sûr : aucune collecte automatique sans action explicite.
        settings.automatic = false;
        let monitors = worker::monitors();
        if settings.screen_index >= monitors.len() {
            settings.screen_index = 0;
        }
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (visit_tx, visit_rx): (Sender<VisitWrite>, Receiver<VisitWrite>) = mpsc::channel();
        std::thread::spawn(move || run_visit_writer(visit_rx));
        worker::spawn(settings.clone(), cmd_rx, event_tx);

        let manager = GlobalHotKeyManager::new().ok();
        let hotkey = shortcut(&settings.hotkey);
        let status = if let Some(m) = &manager {
            match m.register(hotkey) {
                Ok(()) => "Prêt — capture automatique désactivée".to_string(),
                Err(e) => {
                    format!("Raccourci indisponible ({e}). Le bouton manuel reste utilisable.")
                }
            }
        } else {
            "Raccourci global indisponible. Le bouton manuel reste utilisable.".into()
        };
        let ocr_status = Self::ocr_status(settings.ocr_exe.as_deref());
        let mut app = Self {
            #[cfg(windows)]
            tray: match tray::Tray::new(cc) {
                Ok(tray) => Some(tray),
                Err(e) => {
                    diagnostics::write("ICONE", &e);
                    None
                }
            },
            settings,
            settings_path: path,
            cmd_tx,
            event_rx,
            shortcut_manager: manager,
            registered_hotkey: hotkey,
            monitors,
            notice: status,
            notice_is_error: false,
            capture_state: CaptureState::Ready,
            gallery: Vec::new(),
            capture_count: None,
            gallery_scan_rx: None,
            gallery_dirty: false,
            gallery_changed_at: Instant::now(),
            thumbs: HashMap::new(),
            selected_visit: None,
            selected_path: None,
            selected_texture: None,
            calibration_image: None,
            calibration_texture: None,
            calibration_start: None,
            calibration_end: None,
            show_calibration: false,
            show_help: false,
            total_saved_session: 0,
            measured_visits: HashSet::new(),
            analysis_seconds: 0.0,
            last_capture: None,
            last_manual_hotkey: Instant::now() - Duration::from_secs(5),
            last_note: String::new(),
            search: String::new(),
            gallery_filter: 0,
            prepared_paths: HashSet::new(),
            stats: index::Stats::default(),
            ignored: 0,
            rejected: 0,
            export_rx: None,
            show_diagnostic: false,
            diagnostic_text: String::new(),
            ocr_status,
            page: Page::Collecte,
            tracking_only_missing: true,
            visits: Vec::new(),
            session_id: format!(
                "{}-{}",
                std::process::id(),
                chrono::Local::now().timestamp_micros()
            ),
            visit_tx,
            history_rx: None,
            confirm_delete_visit: None,
            confirm_clear_history: None,
            dismissed_visit_ids: HashSet::new(),
        };
        // L'index et la galerie sont chargees sur un thread distinct ;
        // si le mode leger est actif, aucun acces aux miniatures.
        app.scan_gallery();
        let _ = index::recover_interrupted(&app.settings.destination);
        app.visits = index::recent_visits(&app.settings.destination, 500).unwrap_or_default();
        app
    }

    fn ocr_status(path: Option<&Path>) -> String {
        match ocr::locate(path) {
            Some(exe) => match ocr::check_languages(&exe) {
                Ok(()) => {
                    if ocr::bundled_path().as_ref() == Some(&exe) {
                        "Tesseract intégré · français + anglais : OK".into()
                    } else {
                        "Tesseract externe · français + anglais : OK".into()
                    }
                }
                Err(e) => format!("OCR incomplet : {e}"),
            },
            None => "Tesseract introuvable".into(),
        }
    }
    fn update_worker(&mut self) {
        // Ne jamais lancer Tesseract sur le thread de l'interface pour un simple
        // changement de réglage. Le statut OCR initial est déjà connu.
        let _ = self
            .cmd_tx
            .send(WorkerCommand::Configure(self.settings.clone()));
        match settings::store(&self.settings_path, &self.settings) {
            Ok(()) => {}
            Err(e) => self.report_error(format!("Impossible d'enregistrer les préférences : {e}")),
        }
    }

    fn mutate_history(&mut self, change: HistoryMutation) {
        if self.history_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cmd = match &change {
            HistoryMutation::Delete(id) => {
                VisitWrite::Delete(self.settings.destination.clone(), *id, tx)
            }
            HistoryMutation::ClearMissing => {
                VisitWrite::ClearMissing(self.settings.destination.clone(), tx)
            }
            HistoryMutation::ClearAll => {
                VisitWrite::ClearAll(self.settings.destination.clone(), tx)
            }
        };
        if self.visit_tx.send(cmd).is_ok() {
            self.history_rx = Some((change, rx));
        } else {
            self.report_error("Impossible de contacter le gestionnaire de l'historique.".into());
        }
    }

    fn check_history_mutation(&mut self) {
        let result = self.history_rx.as_ref().map(|(_, rx)| rx.try_recv());
        match result {
            Some(Ok(Ok(ids))) => {
                self.history_rx.take();
                let count = ids.len();
                self.dismissed_visit_ids.extend(ids);
                self.visits
                    .retain(|v| !self.dismissed_visit_ids.contains(&v.id));
                if self
                    .selected_visit
                    .as_ref()
                    .is_some_and(|v| self.dismissed_visit_ids.contains(&v.id))
                {
                    self.selected_visit = None;
                    self.selected_texture = None;
                    self.selected_path = None;
                }
                self.notice=format!("Historique mis à jour : {count} entrée(s) retirée(s). Les PNG restent intacts.");
                self.notice_is_error = false;
            }
            Some(Ok(Err(e))) => {
                self.history_rx = None;
                self.report_error(format!("Modification de l'historique impossible : {e}"));
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.history_rx = None;
                self.report_error("Le gestionnaire de l'historique s'est interrompu.".into());
            }
            _ => {}
        }
    }

    fn history_confirmation(&mut self, ctx: &egui::Context) {
        if self.confirm_delete_visit.is_none() && self.confirm_clear_history.is_none() {
            return;
        }
        let is_delete = self.confirm_delete_visit.is_some();
        let delete_all = self.confirm_clear_history == Some(true);
        let title = if is_delete {
            "Retirer une fiche du suivi"
        } else if delete_all {
            "Réinitialiser l'historique de collecte"
        } else {
            "Vider la liste à reprendre"
        };
        let mut open = true;
        let mut do_confirm = false;
        let mut cancel = false;
        egui::Window::new(title).collapsible(false).resizable(false)
            .open(&mut open).show(ctx,|ui|{
                ui.label(if is_delete {
                    "Cette entrée sera retirée de l'historique."
                } else if delete_all {
                    "Toutes les entrées de l'historique de ce dossier seront retirées."
                } else {
                    "Seules les fiches à reprendre et en attente seront retirées."
                });
                ui.colored_label(Color32::from_rgb(128,220,198),
                    "Les PNG enregistrés et les images de contrôle restent conservés sur le disque.");
                ui.add_space(8.0);
                ui.horizontal(|ui|{
                    if ui.button("Annuler").clicked(){cancel=true;}
                    if ui.add(egui::Button::new("Confirmer la suppression")
                        .fill(Color32::from_rgb(145,65,61))).clicked(){do_confirm=true;}
                });
            });
        if do_confirm {
            if let Some(id) = self.confirm_delete_visit.take() {
                self.mutate_history(HistoryMutation::Delete(id));
            } else if self.confirm_clear_history.take() == Some(true) {
                self.mutate_history(HistoryMutation::ClearAll);
            } else {
                self.mutate_history(HistoryMutation::ClearMissing);
            }
            self.confirm_clear_history = None;
        } else if cancel || !open {
            self.confirm_delete_visit = None;
            self.confirm_clear_history = None;
        }
    }

    fn report_error(&mut self, msg: String) {
        diagnostics::write("ERREUR", &msg);
        self.notice = msg;
        self.notice_is_error = true;
    }

    fn mark_gallery_dirty(&mut self) {
        self.gallery_dirty = true;
        self.gallery_changed_at = Instant::now();
    }

    fn scan_gallery(&mut self) {
        self.gallery_dirty = true;
        self.gallery_changed_at = Instant::now();
        self.start_gallery_scan();
    }

    fn refresh_gallery(&mut self) {
        // Discard a scan started before files were moved, including its preview.
        self.gallery_scan_rx = None;
        self.gallery.clear();
        self.capture_count = None;
        self.thumbs.clear();
        self.selected_path = None;
        self.selected_texture = None;
        self.selected_visit = None;
        self.gallery_filter = 0;
        self.scan_gallery();
    }

    fn refresh_gallery_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(
                self.export_rx.is_none(),
                egui::Button::new("Actualiser la galerie"),
            )
            .clicked()
        {
            self.refresh_gallery();
        }
    }

    fn start_gallery_scan(&mut self) {
        if self.gallery_scan_rx.is_some() || !self.gallery_dirty {
            return;
        }
        self.gallery_dirty = false;
        let folder = self.settings.destination.clone();
        let gallery_enabled = self.settings.show_gallery;
        let (tx, rx) = mpsc::channel();
        self.gallery_scan_rx = Some(rx);
        std::thread::spawn(move || {
            if gallery_enabled {
                if let Err(e) = index::sync(&folder) {
                    diagnostics::write("INDEX", &e);
                }
            }
            let listing = duplicates::png_files(&folder);
            let count = listing.as_ref().ok().map(Vec::len);
            let mut files = listing.unwrap_or_default();
            let (stats, prepared) = if gallery_enabled {
                files.sort_by_key(|path| {
                    std::cmp::Reverse(std::fs::metadata(path).and_then(|m| m.modified()).ok())
                });
                (
                    index::stats(&folder).unwrap_or_default(),
                    index::prepared_paths(&folder).unwrap_or_default(),
                )
            } else {
                files.clear();
                (index::Stats::default(), HashSet::new())
            };
            let _ = tx.send(GallerySnapshot {
                folder,
                count,
                gallery_enabled,
                files,
                stats,
                prepared,
            });
        });
    }

    fn check_gallery_scan(&mut self) {
        let result = self.gallery_scan_rx.as_ref().map(|rx| rx.try_recv());
        match result {
            Some(Ok(snapshot)) => {
                self.gallery_scan_rx = None;
                if snapshot.folder == self.settings.destination
                    && snapshot.gallery_enabled == self.settings.show_gallery
                {
                    self.capture_count = snapshot.count;
                    self.gallery = snapshot.files;
                    self.stats = snapshot.stats;
                    self.prepared_paths = snapshot.prepared;
                    self.thumbs.retain(|path, _| self.gallery.contains(path));
                } else {
                    self.gallery_dirty = true;
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.gallery_scan_rx = None;
                self.gallery_dirty = true;
            }
            _ => {}
        }
        // Grouper les sauvegardes rapprochees pour eviter un scan par PNG.
        if self.gallery_scan_rx.is_none()
            && self.gallery_dirty
            && self.gallery_changed_at.elapsed() >= Duration::from_millis(450)
        {
            self.start_gallery_scan();
        }
    }

    fn toggle_gallery(&mut self) {
        self.settings.show_gallery = !self.settings.show_gallery;
        if !self.settings.show_gallery {
            // Relacher immediatement les textures GPU et images en memoire.
            self.thumbs.clear();
            self.selected_texture = None;
            self.selected_path = None;
            self.gallery.clear();
            self.prepared_paths.clear();
            self.gallery_dirty = true;
        }
        self.gallery_scan_rx = None;
        self.scan_gallery();
        // Preference purement visuelle : NE PAS reconfigurer le worker OCR.
        if let Err(e) = settings::store(&self.settings_path, &self.settings) {
            self.report_error(format!("Enregistrement du mode galerie : {e}"));
        }
    }

    fn archive_capture(&mut self, path: &Path) -> Result<(), String> {
        if path.parent() != Some(self.settings.destination.as_path()) {
            return Err("Hors du dossier autorisé".into());
        }
        let folder = self.settings.destination.join("Corbeille_captures");
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let name = path.file_name().ok_or("Sans nom")?;
        let mut target = folder.join(name);
        let mut i = 2;
        while target.exists() {
            target = folder.join(format!("{i}_{}", name.to_string_lossy()));
            i += 1;
        }
        std::fs::rename(path, &target).map_err(|e| e.to_string())?;
        if let Err(e) = index::sync(&self.settings.destination) {
            diagnostics::write("INDEX", &e);
        }
        Ok(())
    }
    fn start_export(&mut self) {
        if self.export_rx.is_some() || self.settings.automatic {
            return;
        }
        let folder = self.settings.destination.clone();
        let chunk_size = self
            .settings
            .split_exports
            .then_some(self.settings.export_chunk_size.max(1));
        let (tx, rx) = mpsc::channel();
        self.export_rx = Some(rx);
        self.notice = "Préparation des fichiers en cours…".into();
        std::thread::spawn(move || {
            let _ = tx.send(index::prepare(&folder, chunk_size));
        });
    }
    fn check_export(&mut self) {
        let event = self.export_rx.as_ref().map(|r| r.try_recv());
        match event {
            Some(Ok(Ok(done))) => {
                self.export_rx = None;
                self.notice = format!(
                    "Préparation terminée : {} PNG déplacés, {} déjà préparés rangés, {} ignorés.",
                    done.moved, done.archived, done.skipped,
                );
                self.notice_is_error = false;
                if done.moved + done.archived > 0 {
                    let _ = Command::new("explorer.exe").arg(&done.path).spawn();
                }
                self.refresh_gallery();
            }
            Some(Ok(Err(e))) => {
                self.export_rx = None;
                self.report_error(e);
                self.refresh_gallery();
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.export_rx = None;
                self.report_error("Préparation interrompue".into());
            }
            _ => {}
        }
    }
    fn diagnostic_window(&mut self, ctx: &egui::Context) {
        if !self.show_diagnostic {
            return;
        }
        let mut open = true;
        egui::Window::new("Diagnostic local")
            .open(&mut open)
            .default_size([720.0, 440.0])
            .show(ctx, |ui| {
                ui.label("Journal des fiches refusées et erreurs. Aucune télémétrie.");
                if ui.button("Actualiser").clicked() {
                    self.diagnostic_text = diagnostics::last_lines(80);
                }
                if ui.button("Ouvrir le journal").clicked() {
                    let p = diagnostics::path();
                    if p.exists() {
                        let _ = Command::new("notepad.exe").arg(p).spawn();
                    }
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.diagnostic_text)
                            .desired_rows(15)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
            });
        if !open {
            self.show_diagnostic = false;
        }
    }

    fn select_file(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self
            .selected_visit
            .as_ref()
            .is_some_and(|v| Path::new(&v.evidence) != path)
        {
            self.selected_visit = None;
        }
        match image::open(&path) {
            Ok(img) => {
                self.selected_texture = Some(picture(
                    ctx,
                    format!("preview:{}", path.display()),
                    img,
                    1400,
                    1300,
                ));
                self.selected_path = Some(path);
            }
            Err(e) => self.report_error(format!("Impossible d'ouvrir l'image : {e}")),
        }
    }

    fn thumbnail(&mut self, ctx: &egui::Context, path: &Path) -> Option<&TextureHandle> {
        if !self.thumbs.contains_key(path) {
            if let Ok(img) = image::open(path) {
                let tex = picture(ctx, format!("thumb:{}", path.display()), img, 190, 170);
                self.thumbs.insert(path.to_path_buf(), tex);
            }
        }
        self.thumbs.get(path)
    }

    fn begin_calibration(&mut self, ctx: &egui::Context) {
        self.calibration_image = None;
        self.calibration_texture = None;
        self.calibration_start = None;
        self.calibration_end = None;
        self.show_calibration = true;
        self.notice_is_error = false;
        self.notice =
            "Capture de calibration en cours : le logiciel se réduit momentanément…".into();
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        let _ = self.cmd_tx.send(WorkerCommand::Calibrate);
    }

    fn change_hotkey(&mut self, previous_label: String) {
        if self.settings.hotkey == previous_label {
            return;
        }
        let error = if let Some(manager) = &self.shortcut_manager {
            let _ = manager.unregister(self.registered_hotkey);
            let proposed = shortcut(&self.settings.hotkey);
            match manager.register(proposed) {
                Ok(()) => {
                    self.registered_hotkey = proposed;
                    None
                }
                Err(e) => {
                    self.settings.hotkey = previous_label;
                    let _ = manager.register(self.registered_hotkey);
                    Some(format!("Impossible d'utiliser ce raccourci : {e}"))
                }
            }
        } else {
            None
        };
        if let Some(message) = error {
            self.report_error(message);
        }
        self.update_worker();
    }

    fn process_events(&mut self, ctx: &egui::Context) {
        self.check_gallery_scan();
        self.check_history_mutation();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == self.registered_hotkey.id()
                && event.state == HotKeyState::Pressed
                && self.last_manual_hotkey.elapsed() > Duration::from_millis(600)
                && self.export_rx.is_none()
            {
                self.last_manual_hotkey = Instant::now();
                let _ = self.cmd_tx.send(WorkerCommand::Manual);
            }
        }
        if self.history_rx.is_some() {
            return;
        }
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                WorkerEvent::Visit {
                    id,
                    name,
                    state,
                    reason,
                    evidence,
                } => {
                    if self.dismissed_visit_ids.contains(&id) {
                        continue;
                    }
                    let old = self.visits.iter().find(|v| v.id == id);
                    let label = name
                        .or_else(|| old.map(|v| v.name.clone()))
                        .unwrap_or_default();
                    let state_changed = old.map(|v| v.state.as_str()) != Some(state);
                    let time = old.map(|v| v.observed_at.clone()).unwrap_or_else(|| {
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
                    });
                    let snapshot = evidence
                        .map(|p| p.to_string_lossy().to_string())
                        .or_else(|| old.map(|v| v.evidence.clone()))
                        .unwrap_or_default();
                    let session_id = old
                        .map(|v| v.session_id.clone())
                        .unwrap_or_else(|| self.session_id.clone());
                    if state == "enregistre" && self.measured_visits.insert(id) {
                        self.analysis_seconds += (chrono::Local::now().timestamp_micros() - id)
                            .max(0) as f64
                            / 1_000_000.0;
                    }
                    let record = index::VisitRecord {
                        id,
                        name: label,
                        state: state.into(),
                        reason,
                        observed_at: time,
                        evidence: snapshot,
                        session_id,
                    };
                    if let Some(existing) = self.visits.iter_mut().find(|v| v.id == id) {
                        *existing = record.clone();
                    } else {
                        self.visits.insert(0, record.clone());
                        self.visits.truncate(500);
                    }
                    // La même fiche peut être rattrapée ensuite par F9 ou une
                    // nouvelle tentative. Ne pas laisser de faux « manquants ».
                    if state == "enregistre" && !record.name.is_empty() {
                        let key = duplicates::normalize_item(&record.name);
                        let mut fixed = Vec::new();
                        for old in self.visits.iter_mut() {
                            if old.id != id
                                && old.state == "a_reprendre"
                                && !old.name.is_empty()
                                && duplicates::normalize_item(&old.name) == key
                            {
                                old.state = "enregistre".into();
                                old.reason = "Rattrapé par une capture ultérieure".into();
                                fixed.push(old.clone());
                            }
                        }
                        for old in fixed {
                            let _ = self
                                .visit_tx
                                .send(VisitWrite::Store(self.settings.destination.clone(), old));
                        }
                    }
                    if state == "a_reprendre" && state_changed && self.settings.alert_sound {
                        #[cfg(windows)]
                        {
                            #[link(name = "user32")]
                            unsafe extern "system" {
                                fn MessageBeep(u_type: u32) -> i32;
                            }
                            unsafe {
                                MessageBeep(0xFFFFFFFF);
                            }
                        }
                    }
                    // SQLite ne doit jamais suspendre la fenêtre ON/OFF.
                    let _ = self
                        .visit_tx
                        .send(VisitWrite::Store(self.settings.destination.clone(), record));
                }
                WorkerEvent::Evidence { id, path } => {
                    if self.dismissed_visit_ids.contains(&id) {
                        continue;
                    }
                    if let Some(v) = self.visits.iter_mut().find(|v| v.id == id) {
                        if v.state == "a_reprendre" && v.evidence.is_empty() {
                            v.evidence = path.to_string_lossy().to_string();
                            let _ = self.visit_tx.send(VisitWrite::Store(
                                self.settings.destination.clone(),
                                v.clone(),
                            ));
                        }
                    }
                }
                WorkerEvent::Saved { path, note } => {
                    self.total_saved_session += 1;
                    self.last_capture = Some(chrono::Local::now());
                    self.notice = format!(
                        "Capture enregistrée : {}",
                        path.file_name()
                            .map(|name| name.to_string_lossy().to_string())
                            .unwrap_or_default()
                    );
                    self.notice_is_error = false;
                    self.last_note = note;
                    // Une reprise manuelle réécrit Nom objet.png : invalider
                    // explicitement les textures pour afficher les nouveaux prix.
                    if self.settings.show_gallery {
                        self.thumbs.remove(&path);
                        if self.selected_path.as_ref() == Some(&path) {
                            // L'aperçu sera recharge uniquement si l'utilisateur
                            // redemande « Voir » : aucun decodage PNG sur sauvegarde.
                            self.selected_texture = None;
                            self.selected_path = None;
                        }
                    }
                    self.mark_gallery_dirty();
                }
                WorkerEvent::Skipped => {
                    self.ignored += 1;
                }
                WorkerEvent::Diagnostic(path) => {
                    self.notice = format!("Capture diagnostic : {}", path.display());
                    let _ = Command::new("explorer.exe").arg(&path).spawn();
                }
                WorkerEvent::State(stage, msg) => {
                    self.capture_state = stage;
                    // Ne pas effacer une erreur utile avec un statut de surveillance
                    // qui arrive aussitôt après. L'utilisateur peut la fermer.
                    if !self.notice_is_error || stage == CaptureState::Incomplete {
                        self.notice = msg;
                        self.notice_is_error = stage == CaptureState::Incomplete;
                    }
                    if stage == CaptureState::Incomplete {
                        self.rejected += 1;
                        diagnostics::write("REFUS", &self.notice);
                    }
                }
                WorkerEvent::Error(msg) => self.report_error(msg),
                WorkerEvent::Calibration(img) => {
                    self.calibration_texture = Some(frame_texture(ctx, "calibration-screen", &img));
                    self.calibration_image = Some(img);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    self.notice = "Indique approximativement la position de la fiche HDV ; aucun contour précis n'est nécessaire.".into();
                    self.notice_is_error = false;
                }
            }
        }
    }

    fn toggle_automatic(&mut self) {
        if self.export_rx.is_some() {
            return;
        }
        self.settings.automatic = !self.settings.automatic;
        if self.settings.automatic {
            self.session_id = format!(
                "{}-{}",
                std::process::id(),
                chrono::Local::now().timestamp_micros()
            );
        }
        self.notice = if self.settings.automatic {
            "Collecte active : parcours les fiches HDV.".into()
        } else {
            "Collecte arrêtée.".into()
        };
        self.notice_is_error = false;
        let _ = self
            .cmd_tx
            .send(WorkerCommand::SetAutomatic(self.settings.automatic));
        if let Err(e) = settings::store(&self.settings_path, &self.settings) {
            self.report_error(format!("Préférences non enregistrées : {e}"));
        }
    }

    fn toggle_compact(&mut self, ctx: &egui::Context) {
        self.settings.compact = !self.settings.compact;
        ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(
            if self.settings.compact {
                egui::vec2(390.0, 315.0)
            } else {
                egui::vec2(980.0, 650.0)
            },
        ));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(if self.settings.compact {
            egui::vec2(490.0, 380.0)
        } else {
            egui::vec2(1480.0, 1000.0)
        }));
        // La vue compacte n'influe pas sur le moteur OCR.
        if let Err(e) = settings::store(&self.settings_path, &self.settings) {
            self.report_error(format!("Affichage non enregistré : {e}"));
        }
    }

    fn header_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui
            .button(if self.settings.compact {
                "Agrandir"
            } else {
                "Mode compact"
            })
            .clicked()
        {
            self.toggle_compact(ctx);
        }
        #[cfg(windows)]
        if self.tray.is_some() && ui.button("Masquer près de l'horloge").clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        ui.add(
            egui::Label::new(
                RichText::new("100 % local · Aucune interaction avec le jeu")
                    .size(12.0)
                    .color(GREEN),
            )
            .wrap_mode(egui::TextWrapMode::Extend),
        );
    }

    fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let brand = |ui: &mut egui::Ui, compact: bool| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 10.0, Color32::from_rgb(12, 49, 65));
            draw_icon(ui.painter(), rect.min + egui::vec2(7.0, 7.0), 34.0, 0, CYAN);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new(APP_TITLE)
                        .size(if compact { 20.0 } else { 26.0 })
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new("Collecte de fiches HDV")
                        .size(12.0)
                        .color(MUTED),
                );
            });
        };
        if self.settings.compact {
            ui.horizontal(|ui| brand(ui, true));
            ui.horizontal_wrapped(|ui| self.header_controls(ui, ctx));
        } else {
            ui.horizontal(|ui| {
                brand(ui, false);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.header_controls(ui, ctx)
                });
            });
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        let missing = self
            .visits
            .iter()
            .filter(|v| v.state != "enregistre")
            .count();
        for (page, icon, label, caption) in [
            (Page::Collecte, 0, "Collecte", "Contrôle et surveillance"),
            (Page::Reprendre, 1, "À reprendre", "Fiches à vérifier"),
            (Page::Captures, 2, "Captures", "Galerie et historique"),
            (Page::Parametres, 3, "Paramètres", "Configuration"),
        ] {
            let chosen = self.page == page;
            let (rect, response) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 76.0), egui::Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::SelectableLabel,
                    ui.is_enabled(),
                    chosen,
                    label,
                )
            });
            let painter = ui.painter_at(rect);
            if chosen || response.hovered() {
                painter.rect_filled(
                    rect,
                    7.0,
                    if chosen {
                        Color32::from_rgb(17, 48, 65)
                    } else {
                        SURFACE
                    },
                );
            }
            if chosen {
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
                    2.0,
                    CYAN,
                );
            }
            let color = if chosen { CYAN } else { MUTED };
            draw_icon(
                &painter,
                rect.min + egui::vec2(18.0, 23.0),
                30.0,
                icon,
                color,
            );
            painter.text(
                rect.min + egui::vec2(66.0, 26.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(16.0),
                if chosen { CYAN } else { TEXT },
            );
            painter.text(
                rect.min + egui::vec2(66.0, 48.0),
                egui::Align2::LEFT_CENTER,
                caption,
                egui::FontId::proportional(12.0),
                MUTED,
            );
            if page == Page::Reprendre && missing > 0 {
                let center = egui::pos2(rect.right() - 23.0, rect.top() + 25.0);
                painter.circle_filled(center, 14.0, Color32::from_rgb(199, 76, 57));
                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    missing.to_string(),
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
            if response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
            {
                self.page = page;
            }
            ui.add_space(5.0);
        }
    }

    fn status_banner(&mut self, ui: &mut egui::Ui) {
        let color = if self.notice_is_error {
            Color32::from_rgb(255, 156, 143)
        } else if self.capture_state == CaptureState::Waiting {
            AMBER
        } else {
            MUTED
        };
        if !self.notice.is_empty() {
            egui::Frame::group(ui.style())
                .fill(SURFACE_ALT)
                .stroke(Stroke::new(1.0_f32, STROKE))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(color, RichText::new(&self.notice).size(13.0));
                        if self.notice_is_error && ui.small_button("Fermer").clicked() {
                            self.notice.clear();
                            self.notice_is_error = false;
                        }
                    });
                });
        }
    }

    fn dashboard(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Collecte").size(25.0).strong().color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let color = if self.settings.automatic {
                    GREEN
                } else {
                    MUTED
                };
                ui.colored_label(
                    color,
                    RichText::new(if self.settings.automatic {
                        "Surveillance active"
                    } else {
                        "Surveillance arrêtée"
                    })
                    .strong(),
                );
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 5.0, color);
            });
        });
        ui.label(
            RichText::new("Surveillance automatique des nouvelles fiches sur l'écran")
                .size(14.0)
                .color(MUTED),
        );
        ui.add_space(17.0);
        let total_width = ui.available_width();
        if total_width >= 730.0 {
            ui.columns(2, |cols| {
                section_card(&mut cols[0], |ui| {
                    ui.set_min_height(76.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Capture automatique")
                                .strong()
                                .size(16.0)
                                .color(TEXT),
                        );
                        if switch(ui, self.settings.automatic, GREEN, "Capture automatique")
                            .on_hover_text("Démarrer ou arrêter la collecte")
                            .clicked()
                        {
                            self.toggle_automatic();
                        }
                    });
                    ui.label(
                        RichText::new(
                            "Détecte et enregistre automatiquement les nouvelles fiches.",
                        )
                        .color(MUTED),
                    );
                });
                section_card(&mut cols[1], |ui| {
                    ui.set_min_height(76.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Capture manuelle")
                                .strong()
                                .size(16.0)
                                .color(TEXT),
                        );
                        if ui
                            .add_sized(
                                [100.0, 34.0],
                                egui::Button::new(RichText::new(&self.settings.hotkey).strong()),
                            )
                            .on_hover_text("Capturer maintenant")
                            .clicked()
                        {
                            let _ = self.cmd_tx.send(WorkerCommand::Manual);
                        }
                    });
                    ui.label(
                        RichText::new("Capturer la fiche affichée actuellement.").color(MUTED),
                    );
                });
            });
        } else {
            section_card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(if self.settings.automatic {
                            "Arrêter la collecte"
                        } else {
                            "Démarrer la collecte"
                        })
                        .clicked()
                    {
                        self.toggle_automatic();
                    }
                    if ui
                        .button(format!("Capturer · {}", self.settings.hotkey))
                        .clicked()
                    {
                        let _ = self.cmd_tx.send(WorkerCommand::Manual);
                    }
                });
            });
        }
        ui.add_space(11.0);
        let recorded = self
            .visits
            .iter()
            .filter(|v| v.state == "enregistre")
            .count();
        let missed = self
            .visits
            .iter()
            .filter(|v| v.state == "a_reprendre")
            .count();
        let pending = self
            .visits
            .iter()
            .filter(|v| v.state == "en_attente")
            .count();
        let average = if self.measured_visits.is_empty() {
            "—".into()
        } else {
            format!(
                "{:.1} s",
                self.analysis_seconds / self.measured_visits.len() as f64
            )
        };
        let last = self
            .last_capture
            .map(|d| d.format("%H:%M").to_string())
            .unwrap_or_else(|| "—".into());
        let metrics = [
            (self.visits.len().to_string(), "Fiches détectées¹", BLUE),
            (recorded.to_string(), "Enregistrées¹", GREEN),
            (missed.to_string(), "À reprendre¹", AMBER),
            (pending.to_string(), "En attente¹", CYAN),
            (average, "Analyse moyenne²", BLUE),
            (last, "Dernière capture²", MUTED),
        ];
        let count = if total_width >= 1000.0 { 6 } else { 3 };
        for group in metrics.chunks(count) {
            ui.columns(count, |cols| {
                for (column, (value, label, color)) in cols.iter_mut().zip(group) {
                    metric(column, value.clone(), label, *color);
                }
            });
            ui.add_space(6.0);
        }
        ui.label(RichText::new("¹ 500 dernières fiches du suivi. ² Session actuelle ; analyse moyenne incluant la file OCR et les doublons déjà présents.").small().color(MUTED));
        ui.add_space(12.0);
        self.review_layout(ui);
        ui.add_space(10.0);
        self.status_banner(ui);
        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Ouvrir les PNG").clicked() {
                let _ = std::fs::create_dir_all(&self.settings.destination);
                let _ = Command::new("explorer.exe")
                    .arg(&self.settings.destination)
                    .spawn();
            }
            self.refresh_gallery_button(ui);
            if ui.button("Contrôle de collecte").clicked() {
                self.page = Page::Reprendre;
            }
            if ui.button("Paramètres avancés").clicked() {
                self.page = Page::Parametres;
            }
        });
        let _ = ctx;
    }

    fn controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Frame::group(ui.style()).fill(SURFACE).stroke(Stroke::new(1.0_f32, STROKE))
            .inner_margin(egui::Margin::same(14)).corner_radius(egui::CornerRadius::same(9)).show(ui, |ui| {
            ui.heading("Configuration de la capture");
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.export_rx.is_none(),
                    egui::Button::new("Capturer maintenant")).clicked() {
                    let _ = self.cmd_tx.send(WorkerCommand::Manual);
                }
                ui.label("Raccourci :");
                let old = self.settings.hotkey.clone();
                egui::ComboBox::from_id_salt("shortcut-picker")
                    .selected_text(&self.settings.hotkey)
                    .show_ui(ui, |ui| {
                        for key in HOTKEYS {
                            ui.selectable_value(&mut self.settings.hotkey, (*key).to_string(), *key);
                        }
                    });
                if old != self.settings.hotkey { self.change_hotkey(old); }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Écran à surveiller :");
                let mut changed = false;
                egui::ComboBox::from_id_salt("screen-picker")
                    .selected_text(self.monitors.get(self.settings.screen_index).map(String::as_str).unwrap_or("Écran 1"))
                    .show_ui(ui, |ui| {
                        for (i, screen) in self.monitors.iter().enumerate() {
                            changed |= ui.selectable_value(&mut self.settings.screen_index, i, screen).changed();
                        }
                    });
                if changed {
                    self.settings.region = None;
                    self.update_worker();
                }
                if ui.button("Indiquer la zone (facultatif)").clicked() { self.begin_calibration(ctx); }
            });
            if let Some(zone) = self.settings.region {
                ui.label(RichText::new(format!("Repère approximatif : x {} · y {} · largeur {} px · hauteur {} px", zone.x, zone.y, zone.width, zone.height))
                    .small().color(Color32::LIGHT_GRAY));
                if ui.small_button("Effacer le repère").clicked() {
                    self.settings.region = None;
                    self.update_worker();
                }
            } else {
                ui.label(RichText::new("Détection autonome : la zone n'est qu'un repère facultatif, pas une bordure à respecter.")
                    .small().color(Color32::from_rgb(235, 192, 129)));
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("Dossier des captures :");
                ui.label(RichText::new(self.settings.destination.to_string_lossy()).small());
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Ouvrir le dossier").clicked() {
                    if let Err(e) = std::fs::create_dir_all(&self.settings.destination) {
                        self.report_error(e.to_string());
                    } else if let Err(e) = Command::new("explorer.exe").arg(&self.settings.destination).spawn() {
                        self.report_error(e.to_string());
                    }
                }
                if ui.add_enabled(self.history_rx.is_none() && self.export_rx.is_none(),
                    egui::Button::new("Changer le dossier")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_directory(&self.settings.destination).pick_folder() {
                        self.settings.destination = dir;
                        self.update_worker();
                        self.gallery.clear();
                        self.thumbs.clear();
                        self.selected_path=None;
                        self.selected_texture=None;
                        self.scan_gallery();
                        self.visits=index::recent_visits(&self.settings.destination,500).unwrap_or_default();
                        self.dismissed_visit_ids.clear();
                    }
                }
                self.refresh_gallery_button(ui);
                if ui.button("Contrôle de collecte").on_hover_text("Objets ouverts, réussites et fiches à reprendre").clicked() {
                    self.page=Page::Reprendre;
                }
                if ui.button("Visualiser la détection").clicked(){let _=self.cmd_tx.send(WorkerCommand::Diagnostic);}
                if ui.button("Journal d'erreurs").on_hover_text("Consulter les détails techniques si une capture échoue.").clicked(){
                    self.show_diagnostic=true;
                    self.diagnostic_text=diagnostics::last_lines(80);
                }
                if ui.add_enabled(self.export_rx.is_none() && !self.settings.automatic,
                    egui::Button::new("Préparer l'envoi")).clicked(){self.start_export();}
            });
            if ui.checkbox(&mut self.settings.alert_sound, "Son discret lors d'une capture manquée")
                .on_hover_text("Désactivé par défaut. Signale les fiches à reprendre détectées pendant la collecte.")
                .changed() {
                if let Err(e)=settings::store(&self.settings_path,&self.settings){
                    self.report_error(format!("Préférence alerte : {e}"));
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("OCR :");
                ui.label(&self.ocr_status);
                ui.label(self.settings.ocr_exe.as_ref()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Détection automatique : moteur intégré, puis installation locale".into()));
                if ui.small_button("Choisir Tesseract.exe").clicked() {
                    if let Some(file) = rfd::FileDialog::new().add_filter("Exécutable Windows", &["exe"]).pick_file() {
                        self.settings.ocr_exe = Some(file);
                        self.ocr_status = Self::ocr_status(self.settings.ocr_exe.as_deref());
                        self.update_worker();
                    }
                }
                if self.settings.ocr_exe.is_some() && ui.small_button("Détection automatique").clicked() {
                    self.settings.ocr_exe = None;
                    self.ocr_status = Self::ocr_status(None);
                    self.update_worker();
                }
            });
        });
        ui.add_space(12.0);
        self.export_settings_ui(ui);
    }

    fn export_settings_ui(&mut self, ui: &mut egui::Ui) {
        section_card(ui, |ui| {
            ui.heading("Préparation de l'envoi");
            let changed = ui
                .add_enabled_ui(self.export_rx.is_none(), |ui| {
                    let mut changed = ui
                        .checkbox(
                            &mut self.settings.split_exports,
                            "Répartir les captures en sous-dossiers numérotés",
                        )
                        .changed();
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Maximum de captures par dossier :");
                        changed |= ui
                            .add_enabled(
                                self.settings.split_exports,
                                egui::DragValue::new(&mut self.settings.export_chunk_size)
                                    .range(1..=usize::MAX)
                                    .speed(1),
                            )
                            .changed();
                    });
                    changed
                })
                .inner;
            ui.label(
                RichText::new(
                    "Les sous-dossiers 1, 2, 3… sont créés à l'intérieur du lot préparé.",
                )
                .small()
                .color(MUTED),
            );
            if changed {
                if let Err(e) = settings::store(&self.settings_path, &self.settings) {
                    self.report_error(format!("Enregistrement des options d'envoi : {e}"));
                }
            }
        });
    }

    fn gallery_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Frame::group(ui.style()).fill(SURFACE).stroke(Stroke::new(1.0_f32, STROKE))
            .inner_margin(egui::Margin::same(14)).corner_radius(egui::CornerRadius::same(9)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(format!("Galerie · {} captures", self.gallery.len()));
                ui.separator();
                ui.label(format!("{} nouvelle(s) depuis le démarrage", self.total_saved_session));
            });
            ui.label(format!("{} objets · {} à préparer · {} préparés",self.stats.total,self.stats.pending,self.stats.prepared));
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.search).hint_text("Rechercher un objet…"));
                egui::ComboBox::from_id_salt("capture-filter").selected_text(match self.gallery_filter {
                    1=>"À préparer",2=>"Préparés",_=>"Tous"
                }).show_ui(ui,|ui|{
                    ui.selectable_value(&mut self.gallery_filter,0,"Tous");
                    ui.selectable_value(&mut self.gallery_filter,1,"À préparer");
                    ui.selectable_value(&mut self.gallery_filter,2,"Préparés");
                });
            });
            ui.label(RichText::new(&self.last_note).small().color(Color32::from_rgb(235, 192, 129)));
            ui.separator();
            if self.gallery.is_empty() {
                ui.add_space(26.0);
                ui.centered_and_justified(|ui| ui.label("Aucune capture pour le moment. Ouvre une fiche d'objet, puis active ON ou le raccourci."));
            } else {
                // Seules les lignes visibles chargent leurs images : une galerie
                // de centaines de PNG ne doit plus figer les boutons.
                let search=duplicates::normalize_item(&self.search);
                let paths=self.gallery.iter().filter(|path| {
                    (search.is_empty() || path.file_stem().is_some_and(|stem|
                        duplicates::normalize_item(&stem.to_string_lossy()).contains(&search)))
                    && match self.gallery_filter {
                        1=>!self.prepared_paths.contains(*path),
                        2=>self.prepared_paths.contains(*path),
                        _=>true,
                    }
                }).take(250).cloned().collect::<Vec<_>>();
                egui::ScrollArea::vertical().max_height(ui.available_height().max(200.0))
                    .show_rows(ui, 106.0, paths.len(), |ui, range| {
                    for path in &paths[range] {
                        // Les actions occupent toujours la colonne à DROITE,
                        // même avec des noms longs et une fenêtre redimensionnée.
                        // La largeur de cette colonne ne dépend pas du texte OCR.
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width().max(180.0), 86.0),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(92.0, 82.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        if ui.add_sized([84.0, 27.0], egui::Button::new("Voir")).clicked() {
                                            self.select_file(ctx, path.clone());
                                        }
                                        if ui.add_enabled_ui(self.export_rx.is_none(), |ui| {
                                            ui.add_sized([84.0, 27.0], egui::Button::new("Archiver"))
                                                .on_hover_text("Déplacer dans la corbeille récupérable")
                                        }).inner.clicked() {
                                            match self.archive_capture(path) {
                                                Ok(()) => {
                                                    if self.selected_path.as_ref() == Some(path) {
                                                        self.selected_path = None;
                                                        self.selected_texture = None;
                                                    }
                                                    self.scan_gallery();
                                                }
                                                Err(e) => self.report_error(format!("Archivage impossible : {e}")),
                                            }
                                        }
                                    },
                                );
                                ui.add_space(8.0);
                                ui.allocate_ui_with_layout(
                                    egui::vec2(ui.available_width().max(70.0), 82.0),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        if let Some(tex) = self.thumbnail(ctx, path) {
                                            let size = tex.size_vec2();
                                            let scale = (104.0 / size.x).min(76.0 / size.y).min(1.0);
                                            let img = egui::Image::new((tex.id(), size * scale));
                                            if ui.add(egui::ImageButton::new(img)).clicked() {
                                                self.select_file(ctx, path.clone());
                                            }
                                        }
                                        let available = ui.available_width().max(40.0);
                                        ui.allocate_ui_with_layout(
                                            egui::vec2(available, 75.0),
                                            egui::Layout::top_down(egui::Align::Min),
                                            |ui| {
                                                ui.add(egui::Label::new(RichText::new(
                                                    path.file_name().map(|name|name.to_string_lossy().to_string()).unwrap_or_default()
                                                ).strong().small()).truncate());
                                                ui.label(RichText::new(if self.prepared_paths.contains(path) {
                                                    "Préparé"
                                                } else {
                                                    "À préparer"
                                                }).small());
                                            },
                                        );
                                    },
                                );
                            },
                        );
                        ui.separator();
                    }
                });
            }
        });
    }

    fn captures_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        title(
            ui,
            "Captures",
            "Galerie locale · Vérifier les PNG avant l'envoi manuel",
        );
        ui.horizontal_wrapped(|ui| {
            ui.label("Galerie + aperçu");
            if switch(ui, self.settings.show_gallery, BLUE, "Galerie et aperçu").clicked() {
                self.toggle_gallery();
            }
            if ui.button("Ouvrir le dossier").clicked() {
                let _ = std::fs::create_dir_all(&self.settings.destination);
                let _ = Command::new("explorer.exe")
                    .arg(&self.settings.destination)
                    .spawn();
            }
            self.refresh_gallery_button(ui);
            if ui
                .add_enabled(
                    self.export_rx.is_none() && !self.settings.automatic,
                    egui::Button::new("Préparer l'envoi"),
                )
                .clicked()
            {
                self.start_export();
            }
        });
        ui.add_space(10.0);
        if self.settings.show_gallery {
            if ui.available_width() > 780.0 {
                ui.columns(2, |cols| {
                    self.gallery_ui(&mut cols[0], ctx);
                    self.preview_ui(&mut cols[1]);
                });
            } else {
                self.gallery_ui(ui, ctx);
                ui.add_space(10.0);
                self.preview_ui(ui);
            }
        } else {
            section_card(ui, |ui| {
                ui.heading("Affichage de la galerie désactivé");
                ui.label("Les captures continuent d'être enregistrées normalement.");
                if ui.button("Réactiver la galerie et l'aperçu").clicked() {
                    self.toggle_gallery();
                }
            });
        }
        self.status_banner(ui);
    }

    fn review_layout(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let counter = self.page == Page::Collecte;
        let side_panel = counter || self.settings.show_gallery;
        if side_panel && width >= 950.0 {
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(width - 294.0, 570.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.tracking_page(ui),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(278.0, 570.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        if counter {
                            self.capture_counter_ui(ui);
                        } else {
                            self.preview_ui(ui);
                        }
                    },
                );
            });
        } else {
            self.tracking_page(ui);
            if side_panel {
                ui.add_space(12.0);
                if counter {
                    self.capture_counter_ui(ui);
                } else {
                    self.preview_ui(ui);
                }
            }
        }
    }

    fn capture_counter_ui(&self, ui: &mut egui::Ui) {
        section_card(ui, |ui| {
            ui.heading("Dans le dossier");
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                let count = self
                    .capture_count
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "…".into());
                ui.label(RichText::new(count).size(64.0).strong().color(CYAN));
                ui.label(RichText::new("captures PNG").color(MUTED));
            });
            ui.add_space(20.0);
            ui.label(
                RichText::new("Les lots préparés sont rangés à part.")
                    .small()
                    .color(MUTED),
            );
        });
    }

    fn tracking_page(&mut self, ui: &mut egui::Ui) {
        let visits = self.visits.clone();
        let missing = visits.iter().filter(|v| v.state != "enregistre").count();
        let can_modify = self.history_rx.is_none();
        section_card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("Fiches à reprendre")
                        .size(21.0)
                        .strong()
                        .color(TEXT),
                );
                ui.colored_label(AMBER, format!("{missing} fiche(s)"));
            });
            ui.label(
                RichText::new("Repérées à partir des captures, à vérifier manuellement.")
                    .color(MUTED),
            );
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Exporter les manquants (CSV)").clicked() {
                    match index::export_missing(&self.settings.destination) {
                        Ok((path, n)) => {
                            self.notice = format!("{n} fiche(s) exportée(s) : {}", path.display());
                            self.notice_is_error = false;
                        }
                        Err(e) => self.report_error(format!("Export impossible : {e}")),
                    }
                }
                if ui
                    .add_enabled(
                        can_modify && missing > 0,
                        egui::Button::new("Vider les manquants"),
                    )
                    .clicked()
                {
                    self.confirm_clear_history = Some(false);
                }
                if ui
                    .add_enabled(
                        can_modify && !visits.is_empty(),
                        egui::Button::new("Effacer l'historique"),
                    )
                    .clicked()
                {
                    self.confirm_clear_history = Some(true);
                }
                ui.checkbox(&mut self.tracking_only_missing, "À vérifier uniquement");
            });
            ui.label(
                RichText::new("500 dernières fiches · Effacer le suivi conserve les images.")
                    .small()
                    .color(MUTED),
            );
            ui.add_space(10.0);
            let visible = visits
                .iter()
                .enumerate()
                .filter(|(_, v)| !self.tracking_only_missing || v.state != "enregistre")
                .map(|(i, _)| i)
                .collect::<Vec<_>>();
            if visible.is_empty() {
                ui.add_space(38.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("Rien à reprendre").size(21.0).color(GREEN));
                    ui.label(RichText::new("Les fiches à vérifier apparaîtront ici.").color(MUTED));
                });
                ui.add_space(38.0);
            } else {
                egui::ScrollArea::horizontal().id_salt("tracking-width").show(ui,|ui| {
                    ui.set_min_width(ui.available_width().max(610.0));
                    let width=ui.available_width();
                    egui::Frame::new().fill(SURFACE_ALT).inner_margin(egui::Margin::symmetric(10,10)).show(ui,|ui| {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(egui::vec2(105.0,20.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_width(105.0);ui.strong("Statut");});
                            ui.allocate_ui_with_layout(egui::vec2((width-310.0).max(260.0),20.0),egui::Layout::top_down(egui::Align::Min),|ui|{ui.set_min_width((width-310.0).max(260.0));ui.strong("Contexte");});
                            ui.label(RichText::new("Actions").strong());
                        });
                    });
                    egui::ScrollArea::vertical().id_salt("tracking-list").max_height((ui.available_height()-20.0).clamp(220.0,520.0))
                        .show_rows(ui,64.0,visible.len(),|ui,range| {
                        for idx in range {
                            let i=visible[idx]; let v=&visits[i];
                            let (state,color)=match v.state.as_str() {"enregistre"=>("Enregistrée",GREEN),"a_reprendre"=>("À reprendre",AMBER),_=>("En attente",BLUE)};
                            let row=egui::Frame::new().inner_margin(egui::Margin::symmetric(10,5)).show(ui,|ui| {
                                ui.set_min_height(48.0);
                                ui.horizontal(|ui| {
                                    ui.add_sized([105.0,42.0],egui::Label::new(RichText::new(state).color(color)));
                                    ui.allocate_ui_with_layout(egui::vec2((width-310.0).max(260.0),48.0),egui::Layout::top_down(egui::Align::Min),|ui| {
                                        ui.set_width((width-310.0).max(260.0));
                                        ui.spacing_mut().item_spacing.y=3.0;
                                        if !v.name.is_empty(){ui.add(egui::Label::new(RichText::new(&v.name).strong()).truncate());}
                                        ui.add(egui::Label::new(if v.reason.is_empty(){"Fiche non identifiée"}else{&v.reason}).truncate());
                                        let n=index::visit_neighbors(&visits,i);
                                        let context=match (n.before,n.after) {
                                            (None,None)=>"Repères de session indisponibles".to_owned(),
                                            (before,after)=>format!("Avant : {} · Après : {}",before.as_deref().unwrap_or("Début"),after.as_deref().unwrap_or("Non identifié")),
                                        };
                                        ui.add(egui::Label::new(RichText::new(context).small().color(MUTED)).truncate());
                                    });
                                    if ui.add(egui::Button::new("Voir").fill(Color32::from_rgb(22,61,94))).clicked(){
                                        self.selected_visit=Some(v.clone());
                                        self.selected_path=None;self.selected_texture=None;
                                        let path=PathBuf::from(&v.evidence);
                                        if path.is_file() {
                                            if self.settings.show_gallery {
                                                self.select_file(ui.ctx(),path);
                                            } else if let Err(e)=Command::new("rundll32.exe").arg("url.dll,FileProtocolHandler").arg(&path).spawn() {
                                                self.report_error(format!("Impossible d'ouvrir l'image : {e}"));
                                            }
                                        } else {
                                            self.notice="Aucune image de contrôle disponible pour cette fiche.".into(); self.notice_is_error=false;
                                        }
                                    }
                                    if ui.add_enabled(can_modify,egui::Button::new("Archiver")).on_hover_text("Retirer du suivi en conservant les images").clicked(){self.confirm_delete_visit=Some(v.id);}
                                });
                            });
                            ui.painter().hline(row.response.rect.x_range(),row.response.rect.bottom()+1.0,Stroke::new(1.0_f32,STROKE));
                            ui.add_space(7.0);
                        }
                    });
                });
            }
        });
        if self.page == Page::Reprendre {
            ui.add_space(10.0);
            self.status_banner(ui);
        }
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui) {
        section_card(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Aperçu").size(20.0).strong());
            ui.add_space(14.0);
            let height = ui.available_width().clamp(180.0, 280.0);
            egui::Frame::new()
                .fill(SURFACE_ALT)
                .stroke(Stroke::new(1.0_f32, STROKE))
                .corner_radius(8)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                        if let Some(tex) = &self.selected_texture {
                            let size = tex.size_vec2();
                            let scale = (ui.available_width() / size.x)
                                .min(height / size.y)
                                .min(1.0);
                            ui.centered_and_justified(|ui| {
                                ui.add(egui::Image::new((tex.id(), size * scale)))
                            });
                        } else {
                            ui.vertical_centered(|ui| {
                                ui.add_space((height - 110.0).max(0.0) / 2.0);
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(52.0, 52.0),
                                    egui::Sense::hover(),
                                );
                                draw_icon(ui.painter(), rect.min, 52.0, 2, MUTED);
                                ui.add_space(16.0);
                                ui.label(
                                    RichText::new(
                                        "Sélectionne une fiche
dans la liste pour l'aperçu",
                                    )
                                    .color(MUTED),
                                );
                            });
                        }
                    });
                });
            ui.add_space(18.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(RichText::new("Détails").size(18.0).strong());
            ui.add_space(8.0);
            if let Some(v) = &self.selected_visit {
                ui.label(if v.name.is_empty() {
                    "Fiche non identifiée"
                } else {
                    &v.name
                });
                ui.label(RichText::new(&v.reason).color(MUTED));
                ui.label(RichText::new(&v.observed_at).small().color(MUTED));
            } else if let Some(path) = &self.selected_path {
                ui.label(path.file_name().unwrap_or_default().to_string_lossy());
            } else {
                ui.label(RichText::new("Aucune fiche sélectionnée.").color(MUTED));
                ui.label(
                    RichText::new("Clique sur Voir pour afficher la capture et ses informations.")
                        .color(MUTED),
                );
            }
            if let Some(path) = &self.selected_path {
                if ui.button("Ouvrir l'image").clicked() {
                    let _ = Command::new("rundll32.exe")
                        .arg("url.dll,FileProtocolHandler")
                        .arg(path)
                        .spawn();
                }
            }
        });
    }

    fn calibration_ui(&mut self, ctx: &egui::Context) {
        if !self.show_calibration {
            return;
        }
        let mut open = true;
        let mut chosen: Option<CaptureRect> = None;
        egui::Window::new("Calibrer le panneau HDV")
            .open(&mut open).resizable(true).default_size([1050.0, 680.0])
            .show(ctx, |ui| {
                ui.label("Ouvre une fiche dans Dofus puis dessine un rectangle approximatif autour de celle-ci. Le logiciel trouve lui-même ses véritables contours et sa hauteur.");
                let Some(tex) = self.calibration_texture.as_ref() else {
                    ui.spinner();
                    ui.label("En attente de la capture de l'écran (la fenêtre se réduit pendant la prise)…");
                    return;
                };
                let Some(frame) = self.calibration_image.as_ref() else { return; };
                let original = egui::vec2(frame.width() as f32, frame.height() as f32);
                let allowed_w = ui.available_width().clamp(200.0, 1050.0);
                let allowed_h = ui.available_height().clamp(150.0, 610.0);
                let scale = (allowed_w / original.x).min(allowed_h / original.y).min(1.0);
                let drawn = original * scale;
                let resp = ui.add(egui::Image::new((tex.id(), drawn)).sense(egui::Sense::click_and_drag()));
                if resp.drag_started() {
                    self.calibration_start = resp.interact_pointer_pos();
                    self.calibration_end = self.calibration_start;
                }
                if resp.dragged() {
                    self.calibration_end = resp.interact_pointer_pos();
                }
                if let (Some(a), Some(b)) = (self.calibration_start, self.calibration_end) {
                    let rect = egui::Rect::from_two_pos(a, b).intersect(resp.rect);
                    let stroke = egui::Stroke::new(2.5_f32, Color32::from_rgb(73, 228, 194));
                    let painter = ui.painter();
                    painter.line_segment([rect.left_top(), rect.right_top()], stroke);
                    painter.line_segment([rect.right_top(), rect.right_bottom()], stroke);
                    painter.line_segment([rect.right_bottom(), rect.left_bottom()], stroke);
                    painter.line_segment([rect.left_bottom(), rect.left_top()], stroke);
                }
                if resp.drag_stopped() {
                    if let (Some(a), Some(b)) = (self.calibration_start, resp.interact_pointer_pos()) {
                        let r = egui::Rect::from_two_pos(a, b).intersect(resp.rect);
                        let x = ((r.left() - resp.rect.left()) / scale).round().max(0.0) as u32;
                        let y = ((r.top() - resp.rect.top()) / scale).round().max(0.0) as u32;
                        let w = (r.width() / scale).round() as u32;
                        let h = (r.height() / scale).round() as u32;
                        if w >= 180 && h >= 190 {
                            chosen = Some(CaptureRect { x, y, width: w, height: h });
                        } else {
                            self.notice = "Zone trop petite. Entoure toute la fiche d'objet.".into();
                        }
                    }
                }
            });
        if let Some(region) = chosen {
            self.settings.region = Some(region);
            self.update_worker();
            self.notice = "Repère enregistré. La fenêtre HDV sera détectée même si le rectangle n'épouse pas ses contours.".into();
            self.notice_is_error = false;
            open = false;
        }
        if !open {
            self.show_calibration = false;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        self.check_export();
        if self.settings.compact {
            egui::CentralPanel::default().show(ctx, |ui| {
                self.header(ui, ctx);
                ui.separator();
                ui.add_space(10.0);
                ui.label(
                    RichText::new(if self.settings.automatic {
                        "Collecte active"
                    } else {
                        "Collecte arrêtée"
                    })
                    .color(if self.settings.automatic {
                        GREEN
                    } else {
                        MUTED
                    })
                    .size(17.0)
                    .strong(),
                );
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(if self.settings.automatic {
                            "Arrêter"
                        } else {
                            "Démarrer"
                        })
                        .clicked()
                    {
                        self.toggle_automatic();
                    }
                    if ui
                        .button(format!("Capturer · {}", self.settings.hotkey))
                        .clicked()
                    {
                        let _ = self.cmd_tx.send(WorkerCommand::Manual);
                    }
                    if ui.button("À reprendre").clicked() {
                        self.toggle_compact(ctx);
                        self.page = Page::Reprendre;
                    }
                });
                ui.label(
                    RichText::new(format!(
                        "{} capture(s) sauvegardée(s) dans cette session",
                        self.total_saved_session
                    ))
                    .color(MUTED),
                );
                self.status_banner(ui);
            });
        } else {
            egui::TopBottomPanel::top("hdv_header")
                .exact_height(78.0)
                .frame(
                    egui::Frame::new()
                        .fill(SURFACE)
                        .inner_margin(egui::Margin::symmetric(18, 9)),
                )
                .show(ctx, |ui| {
                    self.header(ui, ctx);
                });
            egui::SidePanel::left("hdv_nav")
                .resizable(false)
                .exact_width(246.0)
                .frame(
                    egui::Frame::new()
                        .fill(Color32::from_rgb(13, 23, 37))
                        .inner_margin(egui::Margin::symmetric(13, 12)),
                )
                .show(ctx, |ui| {
                    self.sidebar(ui);
                });
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::new()
                        .fill(BACKGROUND)
                        .inner_margin(egui::Margin::same(18)),
                )
                .show(ctx, |ui| {
                    if self.page == Page::Reprendre {
                        egui::ScrollArea::vertical()
                            .id_salt("review-page")
                            .show(ui, |ui| self.review_layout(ui));
                    } else if self.page == Page::Captures {
                        self.captures_page(ui, ctx);
                    } else {
                        egui::ScrollArea::vertical()
                            .id_salt("main-page")
                            .auto_shrink([false, false])
                            .show(ui, |ui| match self.page {
                                Page::Collecte => self.dashboard(ui, ctx),
                                Page::Reprendre => {}
                                Page::Captures => {}
                                Page::Parametres => {
                                    title(
                                        ui,
                                        "Paramètres",
                                        "Raccourcis, écran, sauvegarde et outils de diagnostic",
                                    );
                                    self.controls(ui, ctx);
                                    ui.add_space(12.0);
                                    self.status_banner(ui);
                                    if ui.button("Guide d'utilisation").clicked() {
                                        self.show_help = true;
                                    }
                                }
                            });
                    }
                });
        }
        if self.show_help {
            egui::Window::new("Guide d'utilisation").open(&mut self.show_help).show(ctx, |ui| {
                ui.label("1. Ouvre Dofus 3 et une fiche d'objet dans l'HDV.");
                ui.label("2. Le détecteur trouve la fiche automatiquement. Un repère de zone est facultatif.");
                ui.label("3. Active la collecte, puis parcours les objets dans Dofus.");
                ui.label("4. Le raccourci choisi permet une capture manuelle.");
                ui.label("5. Vérifie les PNG dans Captures avant leur import manuel.");
                ui.label("Désactiver la galerie évite de charger miniatures et aperçus pendant la collecte.");
                ui.separator();
                ui.label("Les offres non visibles à l'écran ne sont pas enregistrées.");
                ui.label("Un PNG par objet : le mode automatique évite les doublons, la capture manuelle remplace l'ancien fichier en conservant une copie récupérable.");
                ui.label("Tesseract fra+eng est nécessaire pour obtenir des noms d'objets complets.");
                ui.label("Pour les noms tronqués, affiche l'infobulle complète en survolant l'objet.");
                ui.label("L'analyse OCR peut se tromper : vérifie les noms et les prix avant tout import.");
            });
        }
        self.calibration_ui(ctx);
        self.diagnostic_window(ctx);
        self.history_confirmation(ctx);
        ctx.request_repaint_after(Duration::from_millis(130));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
        if let Some(manager) = &self.shortcut_manager {
            let _ = manager.unregister(self.registered_hotkey);
        }
    }
}

fn main() -> eframe::Result<()> {
    let compact = settings::load(&settings::config_path()).compact;
    let icon = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .ok()
        .map(|image| {
            let image = image.to_rgba8();
            egui::IconData {
                width: image.width(),
                height: image.height(),
                rgba: image.into_raw(),
            }
        });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_always_on_top()
        .with_inner_size(if compact {
            [490.0, 380.0]
        } else {
            [1480.0, 1000.0]
        })
        .with_min_inner_size(if compact {
            [390.0, 315.0]
        } else {
            [980.0, 650.0]
        });
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod live_history_tests {
    use super::*;

    #[test]
    fn late_ocr_cannot_restore_deleted_rows_and_new_collection_still_works() {
        let folder = std::env::temp_dir().join(format!("hdv-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let png = folder.join("conserve.png");
        std::fs::write(&png, b"unchanged-test-image").unwrap();
        let base = chrono::Local::now().timestamp_micros();
        let record = |offset, state: &str| index::VisitRecord {
            id: base + offset,
            name: format!("Objet de test {offset}"),
            state: state.into(),
            reason: String::new(),
            observed_at: String::new(),
            evidence: String::new(),
            session_id: "test".into(),
        };
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::spawn(move || run_visit_writer(receiver));
        sender
            .send(VisitWrite::Store(folder.clone(), record(1, "enregistre")))
            .unwrap();
        sender
            .send(VisitWrite::Store(folder.clone(), record(2, "en_attente")))
            .unwrap();
        let (ack, reply) = mpsc::channel();
        sender
            .send(VisitWrite::ClearMissing(folder.clone(), ack))
            .unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
            vec![base + 2]
        );
        // Simulate late OCR for the cleared observation, then a genuinely new one.
        sender
            .send(VisitWrite::Store(folder.clone(), record(2, "a_reprendre")))
            .unwrap();
        sender
            .send(VisitWrite::Store(folder.clone(), record(3, "en_attente")))
            .unwrap();
        let (ack, reply) = mpsc::channel();
        sender
            .send(VisitWrite::Delete(folder.clone(), base + 1, ack))
            .unwrap();
        reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        sender
            .send(VisitWrite::Store(folder.clone(), record(1, "enregistre")))
            .unwrap();
        let (ack, reply) = mpsc::channel();
        sender
            .send(VisitWrite::ClearAll(folder.clone(), ack))
            .unwrap();
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
            vec![base + 3]
        );
        sender
            .send(VisitWrite::Store(folder.clone(), record(3, "enregistre")))
            .unwrap();
        sender
            .send(VisitWrite::Store(folder.clone(), record(4, "en_attente")))
            .unwrap();
        drop(sender);
        thread.join().unwrap();
        let rows = index::recent_visits(&folder, 500).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, base + 4);
        assert_eq!(std::fs::read(png).unwrap(), b"unchanged-test-image");
    }
}
