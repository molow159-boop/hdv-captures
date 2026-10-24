//! Preferences and calibration panels.
use super::*;
impl App {
    pub(super) fn controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Frame::group(ui.style()).fill(SURFACE).stroke(Stroke::new(1.0_f32, STROKE))
            .inner_margin(egui::Margin::same(14)).corner_radius(egui::CornerRadius::same(9)).show(ui, |ui| {
            ui.heading("Configuration de la capture");
            ui.add_space(3.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.export_rx.is_none(),
                    egui::Button::new("Capturer maintenant")).clicked() {
                    self.request_manual();
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
                if ui.add_enabled(self.history_rx.is_none() && !self.pipeline_busy() && self.runtime.pending.load(std::sync::atomic::Ordering::SeqCst)==0,
                    egui::Button::new("Changer le dossier")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_directory(&self.settings.destination).pick_folder() {
                        self.settings.destination = dir;
                        self.update_worker();
                        self.gallery.clear();
                        self.thumbs.clear();
                        self.selected_path=None;
                        self.selected_texture=None;
                        self.scan_gallery();
                        self.load_history();
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
                if ui.add_enabled(!self.pipeline_busy(),
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
                ui.label(self.ocr_status.to_string());
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

    pub(super) fn export_settings_ui(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn calibration_ui(&mut self, ctx: &egui::Context) {
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
