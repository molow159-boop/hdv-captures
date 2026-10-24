#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]
use crate::error::AppError;

mod app_events;
mod app_history;
mod batches;
mod diagnostics;
mod duplicates;
mod error;
mod event_pump;
mod file_ops;
mod gallery_controller;
mod history_writer;
mod index;
mod instance;
mod ocr;
mod profiles;
mod settings;
mod state;
mod storage;
mod tools_ui;
mod tray;
mod ui_components;
mod ui_dashboard;
mod ui_pages;
mod ui_settings;
mod vision;
mod worker;

use eframe::egui::{self, Color32, ColorImage, RichText, Stroke, TextureHandle, TextureOptions};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use history_writer::{run_visit_writer, HistoryMutation, VisitWrite};
use image::{DynamicImage, RgbaImage};
use settings::{CaptureRect, Settings};
use state::{GalleryFilter, OcrStatus, ProcessingPhase, VisitState};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use ui_components::*;
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
    Lots,
    Outils,
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

struct GallerySnapshot {
    folder: PathBuf,
    count: Option<usize>,
    gallery_enabled: bool,
    errors: Vec<AppError>,
    files: Vec<PathBuf>,
    stats: Option<index::Stats>,
    prepared: Option<HashSet<PathBuf>>,
}

type HistoryReply = Receiver<Result<Vec<i64>, AppError>>;

struct App {
    #[cfg(windows)]
    tray: Option<tray::Tray>,
    runtime: worker::Runtime,
    pump: event_pump::Pump,
    tools: tools_ui::ToolsState,
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
    last_capture: Option<chrono::DateTime<chrono::Local>>,
    last_note: String,
    search: String,
    gallery_filter: GalleryFilter,
    prepared_paths: HashSet<PathBuf>,
    stats: index::Stats,
    ignored: usize,
    rejected: usize,
    export_rx: Option<Receiver<Result<index::ExportResult, AppError>>>,
    show_diagnostic: bool,
    diagnostic_text: String,
    ocr_status: OcrStatus,
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

        diagnostics::write("DEMARRAGE", "Initialisation des paramètres");
        let path = settings::config_path();
        let (mut settings, config_warning) = settings::load_checked(&path);
        settings.runtime_session = format!(
            "{}-{}",
            std::process::id(),
            chrono::Local::now().timestamp_micros()
        );
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
        let runtime = worker::spawn(settings.clone(), cmd_rx, event_tx);

        let manager = GlobalHotKeyManager::new().ok();
        let hotkey = shortcut(&settings.hotkey);
        let (status, shortcut_error) = if let Some(m) = &manager {
            match m.register(hotkey) {
                Ok(()) => ("Prêt — capture automatique désactivée".to_string(), false),
                Err(e) => (
                    format!("Raccourci indisponible ({e}). Le bouton manuel reste utilisable."),
                    true,
                ),
            }
        } else {
            (
                "Raccourci global indisponible. Le bouton manuel reste utilisable.".into(),
                true,
            )
        };
        let (ui_tx, ui_rx) = mpsc::channel();
        let pump = event_pump::spawn(
            event_rx,
            ui_tx,
            visit_tx.clone(),
            cmd_tx.clone(),
            cc.egui_ctx.clone(),
            hotkey.id(),
        );
        diagnostics::write("DEMARRAGE", "Vérification du moteur OCR");
        let ocr_status = Self::ocr_status(settings.ocr_exe.as_deref());
        diagnostics::write("DEMARRAGE", "Création de la fenêtre et des outils");
        let mut app = Self {
            #[cfg(windows)]
            tray: match tray::Tray::new(cc) {
                Ok(tray) => Some(tray),
                Err(e) => {
                    diagnostics::write("ICONE", &e);
                    None
                }
            },
            tools: tools_ui::ToolsState::new(config_warning),
            runtime,
            pump,
            settings,
            settings_path: path,
            cmd_tx,
            event_rx: ui_rx,
            shortcut_manager: manager,
            registered_hotkey: hotkey,
            monitors,
            notice: status,
            notice_is_error: shortcut_error,
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
            last_capture: None,
            last_note: String::new(),
            search: String::new(),
            gallery_filter: GalleryFilter::All,
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
        app.load_history();
        diagnostics::write("DEMARRAGE", "Interface prête");
        app
    }

    fn ocr_status(path: Option<&Path>) -> OcrStatus {
        match ocr::locate(path) {
            Some(exe) => match ocr::check_languages(&exe) {
                Ok(()) => {
                    if ocr::bundled_path().as_ref() == Some(&exe) {
                        OcrStatus::Bundled
                    } else {
                        OcrStatus::External
                    }
                }
                Err(e) => OcrStatus::Incomplete(e),
            },
            None => OcrStatus::Missing,
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

    fn report_error(&mut self, error: impl std::fmt::Display) {
        let msg = error.to_string();
        diagnostics::write("ERREUR", &msg);
        self.notice = msg;
        self.notice_is_error = true;
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

    fn select_file(&mut self, _ctx: &egui::Context, path: PathBuf) {
        if self
            .selected_visit
            .as_ref()
            .is_some_and(|v| Path::new(&v.evidence) != path)
        {
            self.selected_visit = None;
        }
        self.selected_path = Some(path.clone());
        self.selected_texture = None;
        let (tx, rx) = mpsc::channel();
        self.tools.preview_rx = Some(rx);
        std::thread::spawn(move || {
            let result = image::open(&path)
                .map(|img| img.thumbnail(1400, 1300))
                .map_err(AppError::from);
            let _ = tx.send((path, result));
        });
    }

    fn thumbnail(&mut self, _ctx: &egui::Context, path: &Path) -> Option<&TextureHandle> {
        if !self.thumbs.contains_key(path) {
            self.queue_thumbnail(path);
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
                    self.pump
                        .hotkey
                        .store(proposed.id(), std::sync::atomic::Ordering::SeqCst);
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

    fn toggle_automatic(&mut self) {
        if self.pipeline_busy() {
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
        self.settings.runtime_session = self.session_id.clone();
        self.update_worker();
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
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_events(ctx);
        self.check_export();
        self.tasks_tick(ctx);
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
                        self.request_manual();
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
                                Page::Lots => self.lots_ui(ui),
                                Page::Outils => self.tools_ui(ui),
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
        ctx.request_repaint_after(
            if self.settings.automatic
                || self
                    .runtime
                    .pending
                    .load(std::sync::atomic::Ordering::SeqCst)
                    > 0
                || self.pipeline_busy()
            {
                Duration::from_millis(100)
            } else {
                Duration::from_millis(250)
            },
        );
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
    #[cfg(windows)]
    let _instance = match instance::Instance::acquire() {
        Ok(Some(instance)) => instance,
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!("Verrou de l’application : {error}");
            return Ok(());
        }
    };
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
