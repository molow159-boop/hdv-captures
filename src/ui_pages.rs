//! Gallery, collection review and image preview panels.
use super::*;
impl App {
    pub(super) fn gallery_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                    GalleryFilter::Pending=>"À préparer",GalleryFilter::All=>"Tous"
                }).show_ui(ui,|ui|{
                    ui.selectable_value(&mut self.gallery_filter,GalleryFilter::All,"Tous");
                    ui.selectable_value(&mut self.gallery_filter,GalleryFilter::Pending,"À préparer");

                });
            });
            ui.label(RichText::new(&self.last_note).small().color(Color32::from_rgb(235, 192, 129)));
            ui.separator();
            self.selection_toolbar(ui);
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
                        GalleryFilter::Pending=>!self.prepared_paths.contains(*path),
                        GalleryFilter::All=>true,
                    }
                }).cloned().collect::<Vec<_>>();
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
                                        if ui.add_enabled_ui(!self.pipeline_busy() && self.runtime.pending.load(std::sync::atomic::Ordering::SeqCst)==0, |ui| {
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
                                        let mut selected = self.tools.selected.contains(path);
                                        if ui.checkbox(&mut selected, "").changed() {if selected {self.tools.selected.insert(path.clone());} else {self.tools.selected.remove(path);}}
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

    pub(super) fn captures_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                .add_enabled(!self.pipeline_busy(), egui::Button::new("Préparer l'envoi"))
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

    pub(super) fn review_layout(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn capture_counter_ui(&self, ui: &mut egui::Ui) {
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

    pub(super) fn tracking_page(&mut self, ui: &mut egui::Ui) {
        let visits = self.visits.clone();
        let missing = visits
            .iter()
            .filter(|v| v.state != VisitState::Saved)
            .count();
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
                    self.export_missing_async();
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
                .filter(|(_, v)| !self.tracking_only_missing || v.state != VisitState::Saved)
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
                            let (state,color)=match v.state {VisitState::Saved=>("Enregistrée",GREEN),VisitState::NeedsReview=>("À reprendre",AMBER),VisitState::Pending=>("En attente",BLUE),VisitState::Unknown(_)=>("État inconnu",AMBER)};
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

    pub(super) fn preview_ui(&mut self, ui: &mut egui::Ui) {
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
}
