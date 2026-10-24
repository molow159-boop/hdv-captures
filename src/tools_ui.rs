//! Local selection, batch management, profiles, storage and diagnostics.
use super::*;
use crate::error::AppError;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{SyncSender, TryRecvError};

type ActionReply = Receiver<Result<String, AppError>>;
type BatchReply = Receiver<(PathBuf, Result<Vec<batches::Batch>, AppError>)>;
type HistoryRead = Receiver<(PathBuf, Result<Vec<index::VisitRecord>, AppError>)>;
type UsageReply = Receiver<(PathBuf, u64, Result<storage::Usage, AppError>)>;
type Thumbnail = (PathBuf, Result<DynamicImage, AppError>);

pub struct ToolsState {
    pub wait_seconds: f64,
    pub ocr_seconds: f64,
    pub save_seconds: f64,
    pub selected: HashSet<PathBuf>,
    pub export_waiting: bool,
    pub export_selection: Option<HashSet<PathBuf>>,
    pub batches_dirty: bool,
    pub phase: ProcessingPhase,
    pub analysis_time: f64,
    pub analysis_count: usize,
    pause: Option<Receiver<()>>,
    closing: bool,
    shutdown_sent: bool,
    flushed: Option<Receiver<()>>,
    allow_close: bool,
    action: Option<ActionReply>,
    batches: Vec<batches::Batch>,
    batch_rx: Option<BatchReply>,
    history_rx: Option<HistoryRead>,
    usage: storage::Usage,
    usage_scanned: bool,
    usage_rx: Option<UsageReply>,
    days: u64,
    confirm_cleanup: bool,
    profiles: Vec<profiles::Profile>,
    profile_name: String,
    warning: Option<String>,
    rename: Option<PathBuf>,
    rename_name: String,
    diagnostic: String,
    thumbs_epoch: u64,
    thumbs_order: std::collections::VecDeque<PathBuf>,
    thumbs_tx: SyncSender<(PathBuf, u64)>,
    thumbs_rx: Receiver<(PathBuf, u64, Result<DynamicImage, AppError>)>,
    thumbs_waiting: HashSet<PathBuf>,
    pub preview_rx: Option<Receiver<Thumbnail>>,
}
impl ToolsState {
    pub fn history_loading(&self) -> bool {
        self.history_rx.is_some()
    }
    pub fn new(warning: Option<String>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<(PathBuf, u64)>(24);
        let (out, results) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok((path, epoch)) = rx.recv() {
                let image = image::open(&path)
                    .map(|img| img.thumbnail(190, 170))
                    .map_err(AppError::from);
                if out.send((path, epoch, image)).is_err() {
                    break;
                }
            }
        });
        let (profiles, warning) = match profiles::load() {
            Ok(p) => (p, warning),
            Err(e) => (Vec::new(), Some(format!("Profils illisibles : {e}"))),
        };
        Self {
            wait_seconds: 0.0,
            ocr_seconds: 0.0,
            save_seconds: 0.0,
            selected: HashSet::new(),
            export_waiting: false,
            export_selection: None,
            batches_dirty: true,
            phase: ProcessingPhase::Ready,
            analysis_time: 0.0,
            analysis_count: 0,
            pause: None,
            closing: false,
            shutdown_sent: false,
            flushed: None,
            allow_close: false,
            action: None,
            batches: Vec::new(),
            batch_rx: None,
            history_rx: None,
            usage: storage::Usage::default(),
            usage_scanned: false,
            usage_rx: None,
            days: 30,
            confirm_cleanup: false,
            profiles,
            profile_name: String::new(),
            warning,
            rename: None,
            rename_name: String::new(),
            diagnostic: String::new(),
            thumbs_epoch: 0,
            thumbs_order: std::collections::VecDeque::new(),
            thumbs_tx: tx,
            thumbs_rx: results,
            thumbs_waiting: HashSet::new(),
            preview_rx: None,
        }
    }
}
impl App {
    pub fn invalidate_thumbnails(&mut self) {
        self.thumbs.clear();
        self.tools.thumbs_epoch = self.tools.thumbs_epoch.wrapping_add(1);
        self.tools.thumbs_waiting.clear();
        self.tools.thumbs_order.clear();
    }
    pub fn pipeline_status(&mut self, ui: &mut egui::Ui) {
        let pending = self.runtime.pending.load(Ordering::SeqCst);
        section_card(ui, |ui| {
            ui.label(format!(
                "{} · {} analyse(s) en cours ou en attente",
                if self.runtime.admitted.load(Ordering::SeqCst) {
                    "Surveillance active"
                } else {
                    "Surveillance arrêtée"
                },
                pending
            ));
            if pending > 0 {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(self.tools.phase.label());
                });
            }
            if self.tools.export_waiting {
                ui.colored_label(AMBER, "Fin des analyses avant préparation de l'envoi…");
            }
            if self.tools.analysis_count > 0 {
                let count = self.tools.analysis_count as f64;
                ui.label(format!(
                    "Moyennes : attente {:.2} s · OCR {:.2} s · sauvegarde {:.2} s",
                    self.tools.wait_seconds / count,
                    self.tools.ocr_seconds / count,
                    self.tools.save_seconds / count
                ));
            }
        });
        ui.add_space(10.0);
    }
    pub fn export_missing_async(&mut self) {
        if self.pipeline_busy() {
            return;
        }
        let folder = self.settings.destination.clone();
        let (tx, rx) = mpsc::channel();
        self.tools.action = Some(rx);
        self.pump.blocked.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let _ = tx.send(
                index::export_missing(&folder)
                    .map(|(path, n)| format!("{n} fiches exportées : {}", path.display())),
            );
        });
    }
    pub fn pipeline_busy(&self) -> bool {
        self.export_rx.is_some()
            || self.tools.export_waiting
            || self.tools.closing
            || self.tools.action.is_some()
            || self.tools.history_rx.is_some()
    }
    pub fn request_manual(&mut self) {
        if !self.pipeline_busy() {
            let _ = self.cmd_tx.send(WorkerCommand::Manual);
        }
    }
    pub fn pause_pipeline(&mut self) {
        self.pump.blocked.store(true, Ordering::SeqCst);
        self.settings.automatic = false;
        let (tx, rx) = mpsc::channel();
        self.tools.pause = Some(rx);
        if self.cmd_tx.send(WorkerCommand::Pause(tx)).is_err() {
            self.report_error("Le moteur de capture est indisponible.");
        }
    }
    pub fn load_history(&mut self) {
        let folder = self.settings.destination.clone();
        let (tx, rx) = mpsc::channel();
        self.tools.history_rx = Some(rx);
        self.visits.clear();
        self.tools.usage_scanned = false;
        self.tools.usage = storage::Usage::default();
        self.tools.selected.clear();
        self.tools.batches_dirty = true;
        std::thread::spawn(move || {
            let result = (|| {
                index::recover_preparations(&folder)?;
                index::recover_interrupted(&folder)?;
                index::recent_visits(&folder, 500)
            })();
            let _ = tx.send((folder, result));
        });
    }
    pub fn persist_late_event(&mut self, _folder: PathBuf, _session: String, event: WorkerEvent) {
        if let WorkerEvent::Error(error) = event {
            diagnostics::write("ANCIEN_DOSSIER", &error);
        }
    }
    pub fn archive_paths(&mut self, paths: Vec<PathBuf>) {
        if self.pipeline_busy() || self.runtime.pending.load(Ordering::SeqCst) > 0 {
            return;
        }
        let folder = self.settings.destination.clone();
        let (tx, rx) = mpsc::channel();
        self.tools.action = Some(rx);
        self.pump.blocked.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let result = crate::file_ops::archive(&folder, &paths).and_then(|count| {
                index::sync(&folder)?;
                Ok(format!(
                    "{count} capture(s) déplacée(s) dans la corbeille récupérable."
                ))
            });
            let _ = tx.send(result);
        });
    }
    pub fn selection_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{} sélectionnée(s)", self.tools.selected.len()));
            if ui.button("Tout sélectionner").clicked() {
                self.tools.selected.extend(self.gallery.iter().cloned());
            }
            if ui.button("Désélectionner").clicked() {
                self.tools.selected.clear();
            }
            let idle = !self.pipeline_busy() && self.runtime.pending.load(Ordering::SeqCst) == 0;
            if ui
                .add_enabled(
                    idle && !self.tools.selected.is_empty(),
                    egui::Button::new("Archiver la sélection"),
                )
                .clicked()
            {
                self.archive_paths(self.tools.selected.iter().cloned().collect());
            }
            if ui
                .add_enabled(
                    idle && self.tools.selected.len() == 1,
                    egui::Button::new("Corriger le nom"),
                )
                .clicked()
            {
                self.tools.rename = self.tools.selected.iter().next().cloned();
                self.tools.rename_name = self
                    .tools
                    .rename
                    .as_ref()
                    .and_then(|p| p.file_stem())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
        });
        if !self.tools.selected.is_empty() {
            ui.label(
                RichText::new("Préparer l'envoi utilisera uniquement la sélection.")
                    .small()
                    .color(CYAN),
            );
        }
    }
    pub fn queue_thumbnail(&mut self, path: &Path) {
        if self.tools.thumbs_waiting.insert(path.to_path_buf())
            && self
                .tools
                .thumbs_tx
                .try_send((path.to_path_buf(), self.tools.thumbs_epoch))
                .is_err()
        {
            self.tools.thumbs_waiting.remove(path);
        }
    }
    fn refresh_batches(&mut self) {
        let folder = self.settings.destination.clone();
        let (tx, rx) = mpsc::channel();
        self.tools.batch_rx = Some(rx);
        self.tools.batches_dirty = false;
        std::thread::spawn(move || {
            let result = batches::list(&folder);
            let _ = tx.send((folder, result));
        });
    }
    pub fn lots_ui(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Lots préparés",
            "Suivi local : marquer un lot envoyé ne transmet aucun fichier.",
        );
        if self.tools.batches_dirty && self.tools.batch_rx.is_none() {
            self.refresh_batches();
        }
        if ui.button("Actualiser les lots").clicked() {
            self.tools.batches_dirty = true;
        }
        let lots = self.tools.batches.clone();
        if lots.is_empty() {
            ui.label("Aucun lot préparé dans ce dossier.");
        }
        for lot in lots {
            section_card(ui, |ui| {
                ui.heading(lot.path.file_name().unwrap_or_default().to_string_lossy());
                ui.label(format!(
                    "{} captures · {}",
                    lot.count,
                    if lot.sent { "Envoyé" } else { "À envoyer" }
                ));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Ouvrir le lot").clicked() {
                        let _ = Command::new("explorer.exe").arg(&lot.path).spawn();
                    }
                    if ui
                        .add_enabled(
                            !self.pipeline_busy(),
                            egui::Button::new(if lot.sent {
                                "Remettre à envoyer"
                            } else {
                                "Marquer envoyé"
                            }),
                        )
                        .clicked()
                    {
                        let folder = self.settings.destination.clone();
                        let (tx, rx) = mpsc::channel();
                        self.tools.action = Some(rx);
                        self.pump.blocked.store(true, Ordering::SeqCst);
                        std::thread::spawn(move || {
                            let _ = tx.send(
                                batches::mark_sent(&folder, &lot.path, !lot.sent)
                                    .map(|_| "État du lot enregistré.".into()),
                            );
                        });
                    }
                });
            });
            ui.add_space(8.0);
        }
    }
    fn inspect_storage(&mut self) {
        let folder = self.settings.destination.clone();
        let days = self.tools.days;
        let (tx, rx) = mpsc::channel();
        self.tools.usage_rx = Some(rx);
        std::thread::spawn(move || {
            let result = storage::inspect(&folder, days);
            let _ = tx.send((folder, days, result));
        });
    }
    pub fn tools_ui(&mut self, ui: &mut egui::Ui) {
        title(
            ui,
            "Outils",
            "Profils locaux, stockage et diagnostic à vérifier avant partage.",
        );
        section_card(ui, |ui| {
            ui.heading("Profils de réglages");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.tools.profile_name)
                        .hint_text("Nom du profil"),
                );
                if ui.button("Enregistrer le profil actuel").clicked() {
                    let name = self.tools.profile_name.trim().to_string();
                    if name.is_empty() {
                        self.report_error("Donne un nom au profil.");
                    } else {
                        self.tools.profiles.retain(|p| p.name != name);
                        self.tools.profiles.push(profiles::Profile {
                            name,
                            settings: self.settings.clone(),
                        });
                        if let Err(e) = profiles::store(&self.tools.profiles) {
                            self.report_error(e);
                        }
                    }
                }
            });
            for profile in self.tools.profiles.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&profile.name);
                    if ui
                        .add_enabled(
                            !self.pipeline_busy()
                                && self.runtime.pending.load(Ordering::SeqCst) == 0,
                            egui::Button::new("Appliquer"),
                        )
                        .clicked()
                    {
                        let previous = self.settings.hotkey.clone();
                        let session = self.settings.runtime_session.clone();
                        self.settings = profile.settings.clone();
                        self.settings.automatic = false;
                        self.settings.runtime_session = session;
                        if self.settings.screen_index >= self.monitors.len() {
                            self.settings.screen_index = 0;
                            self.settings.region = None;
                        }
                        self.change_hotkey(previous);
                        self.update_worker();
                        self.refresh_gallery();
                        self.load_history();
                        self.ocr_status = Self::ocr_status(self.settings.ocr_exe.as_deref());
                    }
                    if ui.button("Retirer le profil").clicked() {
                        self.tools.profiles.retain(|p| p.name != profile.name);
                        if let Err(e) = profiles::store(&self.tools.profiles) {
                            self.report_error(e);
                        }
                    }
                });
            }
        });
        ui.add_space(12.0);
        section_card(ui, |ui| {
            ui.heading("Stockage du dossier de captures");
            if !self.tools.usage_scanned {
                ui.label("Clique sur Analyser le stockage pour mesurer ce dossier.");
            }
            ui.label(format!(
                "{} fichiers · {:.1} Mio",
                self.tools.usage.files,
                self.tools.usage.bytes as f64 / 1048576.0
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label("Archives de plus de (jours)");
                if ui
                    .add(egui::DragValue::new(&mut self.tools.days).range(1..=3650))
                    .changed()
                {
                    self.tools.usage.cleanup.clear();
                    self.tools.usage_scanned = false;
                }
                if ui
                    .add_enabled(
                        self.tools.usage_rx.is_none(),
                        egui::Button::new("Analyser le stockage"),
                    )
                    .clicked()
                {
                    self.inspect_storage();
                }
            });
            ui.label("Le nettoyage concerne uniquement les anciennes images de contrôle, captures remplacées et corbeille. Les captures actives et les lots sont conservés.");
            ui.label(format!(
                "{} anciennes images dans la dernière analyse",
                self.tools.usage.cleanup.len()
            ));
            if ui
                .add_enabled(
                    !self.tools.usage.cleanup.is_empty()
                        && !self.pipeline_busy()
                        && self.runtime.pending.load(Ordering::SeqCst) == 0,
                    egui::Button::new("Examiner le nettoyage"),
                )
                .clicked()
            {
                self.tools.confirm_cleanup = true;
            }
        });
        ui.add_space(12.0);
        section_card(ui, |ui| {
            ui.heading("Diagnostic à partager");
            ui.label("Aucune capture, aucun nom d'objet et aucun chemin personnel n'est ajouté automatiquement. Relis le texte avant de l'enregistrer.");
            if ui.button("Préparer le diagnostic").clicked() {
                self.tools.diagnostic=format!("HDV Captures {}\nWindows x64\nOCR : {}\nCaptures du dossier : {}\nAnalyses en attente : {}\nTraitement moyen hors file : {:.3} s\n\nDescription du problème :\n",env!("CARGO_PKG_VERSION"),self.ocr_status,self.capture_count.unwrap_or(0),self.runtime.pending.load(Ordering::SeqCst),if self.tools.analysis_count==0{0.0}else{self.tools.analysis_time/self.tools.analysis_count as f64});
            }
            ui.add(
                egui::TextEdit::multiline(&mut self.tools.diagnostic)
                    .desired_rows(9)
                    .desired_width(f32::INFINITY),
            );
            if ui
                .add_enabled(
                    !self.tools.diagnostic.is_empty(),
                    egui::Button::new("Enregistrer ce texte…"),
                )
                .clicked()
            {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("Diagnostic-HDV-Captures.txt")
                    .save_file()
                {
                    match std::fs::write(path, &self.tools.diagnostic) {
                        Ok(()) => {
                            self.notice =
                                "Diagnostic enregistré localement. Aucun envoi effectué.".into();
                        }
                        Err(e) => self.report_error(e.to_string()),
                    }
                }
            }
        });
    }
    pub fn tasks_tick(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.tools.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.tools.closing {
                self.pause_pipeline();
                self.tools.closing = true;
                self.tools.export_waiting = false;
            }
        }
        if self
            .tools
            .pause
            .as_ref()
            .is_some_and(|rx| matches!(rx.try_recv(), Ok(()) | Err(TryRecvError::Disconnected)))
        {
            self.tools.pause = None;
        }
        let empty = self.runtime.pending.load(Ordering::SeqCst) == 0 && self.tools.pause.is_none();
        if self.tools.export_waiting && empty {
            self.tools.export_waiting = false;
            self.launch_export();
        }
        if self.tools.closing {
            self.notice = format!(
                "Fermeture après les opérations en cours · {} analyse(s) restante(s)",
                self.runtime.pending.load(Ordering::SeqCst)
            );
            if empty
                && self.export_rx.is_none()
                && self.tools.action.is_none()
                && self.history_rx.is_none()
                && self.gallery_scan_rx.is_none()
                && self.tools.history_rx.is_none()
                && self.tools.batch_rx.is_none()
                && self.tools.usage_rx.is_none()
            {
                if !self.tools.shutdown_sent {
                    let _ = self.cmd_tx.send(WorkerCommand::Shutdown);
                    self.tools.shutdown_sent = true;
                } else if self.runtime.finished.load(Ordering::SeqCst)
                    && self.pump.finished.load(Ordering::SeqCst)
                {
                    if self.tools.flushed.is_none() {
                        let (tx, rx) = mpsc::channel();
                        let _ = self.visit_tx.send(VisitWrite::Flush(tx));
                        self.tools.flushed = Some(rx);
                    } else if self.tools.flushed.as_ref().is_some_and(|rx| {
                        matches!(rx.try_recv(), Ok(()) | Err(TryRecvError::Disconnected))
                    }) {
                        self.tools.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        while let Ok((path, epoch, result)) = self.tools.thumbs_rx.try_recv() {
            if epoch != self.tools.thumbs_epoch {
                continue;
            }
            self.tools.thumbs_waiting.remove(&path);
            if self.settings.show_gallery && self.gallery.contains(&path) {
                if let Ok(img) = result {
                    while self.thumbs.len() >= 128 {
                        if let Some(old) = self.tools.thumbs_order.pop_front() {
                            self.thumbs.remove(&old);
                        } else {
                            break;
                        }
                    }
                    self.tools.thumbs_order.push_back(path.clone());
                    self.thumbs.insert(
                        path.clone(),
                        picture(ctx, format!("thumb:{}", path.display()), img, 190, 170),
                    );
                }
            }
        }
        if let Some(Ok((path, result))) = self.tools.preview_rx.as_ref().map(|rx| rx.try_recv()) {
            self.tools.preview_rx = None;
            if self.selected_path.as_ref() == Some(&path) {
                match result {
                    Ok(img) => {
                        self.selected_texture = Some(picture(
                            ctx,
                            format!("preview:{}", path.display()),
                            img,
                            1400,
                            1300,
                        ))
                    }
                    Err(e) => self.report_error(e),
                }
            }
        }
        if let Some(result) = self
            .tools
            .action
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(r) => Some(r),
                Err(TryRecvError::Disconnected) => Some(Err(AppError::Domain(
                    crate::error::DomainError::ActionInterrupted,
                ))),
                _ => None,
            })
        {
            self.tools.action = None;
            match result {
                Ok(message) => {
                    self.notice = message;
                    self.notice_is_error = false;
                }
                Err(e) => self.report_error(e),
            }
            self.tools.selected.clear();
            self.tools.batches_dirty = true;
            self.refresh_gallery();
            if self.page == Page::Outils {
                self.inspect_storage();
            }
        }
        if let Some(Ok((folder, result))) = self.tools.batch_rx.as_ref().map(|rx| rx.try_recv()) {
            self.tools.batch_rx = None;
            if folder == self.settings.destination {
                match result {
                    Ok(lots) => self.tools.batches = lots,
                    Err(e) => self.report_error(e),
                }
            } else {
                self.tools.batches_dirty = true;
            }
        }
        if let Some(Ok((folder, result))) = self.tools.history_rx.as_ref().map(|rx| rx.try_recv()) {
            self.tools.history_rx = None;
            if folder == self.settings.destination {
                match result {
                    Ok(visits) => {
                        for record in visits {
                            if !self.visits.iter().any(|v| v.id == record.id) {
                                self.visits.push(record);
                            }
                        }
                        self.visits.sort_by_key(|v| std::cmp::Reverse(v.id));
                        self.visits.truncate(500);
                        self.scan_gallery();
                    }
                    Err(e) => self.report_error(e),
                }
            }
        }
        if let Some(Ok((folder, days, result))) =
            self.tools.usage_rx.as_ref().map(|rx| rx.try_recv())
        {
            self.tools.usage_rx = None;
            if folder == self.settings.destination && days == self.tools.days {
                match result {
                    Ok(usage) => {
                        self.tools.usage = usage;
                        self.tools.usage_scanned = true;
                    }
                    Err(e) => self.report_error(e),
                }
            }
        }
        self.pump
            .blocked
            .store(self.pipeline_busy(), Ordering::SeqCst);
        self.tools_dialogs(ctx);
    }
    fn tools_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(warning) = self.tools.warning.clone() {
            egui::Window::new("Réglages récupérés")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(warning);
                    if ui.button("Compris").clicked() {
                        self.tools.warning = None;
                    }
                });
        }
        if self.tools.rename.is_some() {
            egui::Window::new("Corriger le nom OCR")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        "Le PNG sera renommé et l'index mis à jour. Aucun doublon n'est écrasé.",
                    );
                    ui.text_edit_singleline(&mut self.tools.rename_name);
                    if ui.button("Annuler").clicked() {
                        self.tools.rename = None;
                    }
                    if ui
                        .add_enabled(
                            !self.pipeline_busy()
                                && self.runtime.pending.load(Ordering::SeqCst) == 0,
                            egui::Button::new("Renommer"),
                        )
                        .clicked()
                    {
                        if let Some(path) = self.tools.rename.take() {
                            let folder = self.settings.destination.clone();
                            let name = self.tools.rename_name.clone();
                            let (tx, rx) = mpsc::channel();
                            self.tools.action = Some(rx);
                            self.pump.blocked.store(true, Ordering::SeqCst);
                            std::thread::spawn(move || {
                                let _ = tx.send(
                                    index::rename_capture(&folder, &path, &name)
                                        .map(|_| "Nom corrigé.".into()),
                                );
                            });
                        }
                    }
                });
        }
        if self.tools.confirm_cleanup {
            egui::Window::new("Confirmer le nettoyage").collapsible(false).show(ctx,|ui|{
                ui.label(format!("Supprimer définitivement {} anciennes images des archives et de la corbeille ?",self.tools.usage.cleanup.len()));
                egui::ScrollArea::vertical().max_height(180.0).show(ui,|ui|{for candidate in &self.tools.usage.cleanup {ui.label(candidate.preview(&self.settings.destination));}});
                ui.label("Les captures actives et les lots préparés ne seront pas supprimés.");
                if ui.button("Annuler").clicked(){self.tools.confirm_cleanup=false;}
                if ui.add_enabled(!self.pipeline_busy()&&self.runtime.pending.load(Ordering::SeqCst)==0,egui::Button::new("Confirmer la suppression")).clicked(){
                    let folder=self.settings.destination.clone();let plan=self.tools.usage.cleanup.clone();let(tx,rx)=mpsc::channel();self.tools.action=Some(rx);self.tools.confirm_cleanup=false;
                    std::thread::spawn(move||{let _=tx.send(storage::cleanup(&folder,&plan).map(|n|format!("{n} anciennes images supprimées.")));});
                    self.tools.usage=storage::Usage::default();
                }
            });
        }
    }
}
