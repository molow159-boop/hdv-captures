//! Header, navigation, collection dashboard and notices.
use super::*;
impl App {
    pub(super) fn header_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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

    pub(super) fn header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let brand = |ui: &mut egui::Ui, compact: bool| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(48.0, 48.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 10.0, Color32::from_rgb(12, 49, 65));
            draw_icon(ui.painter(), rect.min + egui::vec2(7.0, 7.0), 34.0, 0, CYAN);
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("HDV Captures")
                        .size(if compact { 20.0 } else { 26.0 })
                        .strong()
                        .color(TEXT),
                );
                ui.label(
                    RichText::new("Collecte de fiches HDV · 1.1.0")
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

    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        let missing = self
            .visits
            .iter()
            .filter(|v| v.state != VisitState::Saved)
            .count();
        for (page, icon, label, caption) in [
            (Page::Collecte, 0, "Collecte", "Contrôle et surveillance"),
            (Page::Reprendre, 1, "À reprendre", "Fiches à vérifier"),
            (Page::Captures, 2, "Captures", "Galerie et historique"),
            (Page::Parametres, 3, "Paramètres", "Configuration"),
            (Page::Lots, 4, "Lots préparés", "À envoyer et envoyés"),
            (Page::Outils, 3, "Outils", "Profils, stockage, diagnostic"),
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

    pub(super) fn status_banner(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn dashboard(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                            self.request_manual();
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
                        self.request_manual();
                    }
                });
            });
        }
        ui.add_space(11.0);
        let recorded = self
            .visits
            .iter()
            .filter(|v| v.state == VisitState::Saved)
            .count();
        let missed = self
            .visits
            .iter()
            .filter(|v| v.state == VisitState::NeedsReview)
            .count();
        let pending = self
            .visits
            .iter()
            .filter(|v| v.state == VisitState::Pending)
            .count();
        let average = if self.tools.analysis_count == 0 {
            "—".into()
        } else {
            format!(
                "{:.1} s",
                self.tools.analysis_time / self.tools.analysis_count as f64
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
        ui.label(RichText::new("¹ 500 dernières fiches du suivi. ² Session actuelle ; traitement OCR mesuré hors attente dans la file.").small().color(MUTED));
        ui.add_space(12.0);
        self.pipeline_status(ui);
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
}
