//! Apply worker events to the visible application state.
use super::*;
impl App {
    pub(super) fn process_events(&mut self, ctx: &egui::Context) {
        self.check_gallery_scan();
        self.check_history_mutation();
        if self.history_rx.is_some() {
            return;
        }
        while let Ok(event) = self.event_rx.try_recv() {
            let (event, session) = match event {
                WorkerEvent::Scoped {
                    folder,
                    session,
                    event,
                } => {
                    if folder != self.settings.destination {
                        self.persist_late_event(folder, session, *event);
                        continue;
                    }
                    (*event, session)
                }
                event => (event, self.session_id.clone()),
            };
            match event {
                WorkerEvent::Scoped { .. } => {}
                WorkerEvent::Timing { waiting, ocr, save } => {
                    self.tools.wait_seconds += waiting.as_secs_f64();
                    self.tools.ocr_seconds += ocr.as_secs_f64();
                    self.tools.save_seconds += save.as_secs_f64();
                }
                WorkerEvent::Progress { phase, elapsed } => {
                    self.tools.phase = phase;
                    if !elapsed.is_zero() {
                        self.tools.analysis_time += elapsed.as_secs_f64();
                        self.tools.analysis_count += 1;
                    }
                }
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
                    let state_changed = old.map(|v| &v.state) != Some(&state);
                    let time = old.map(|v| v.observed_at.clone()).unwrap_or_else(|| {
                        chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
                    });
                    let snapshot = evidence
                        .map(|p| p.to_string_lossy().to_string())
                        .or_else(|| old.map(|v| v.evidence.clone()))
                        .unwrap_or_default();
                    let session_id = old.map(|v| v.session_id.clone()).unwrap_or(session);

                    let record = index::VisitRecord {
                        id,
                        name: label,
                        state: state.clone(),
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
                    // La même fiche peut être rattrapée ensuite par raccourci manuel ou une
                    // nouvelle tentative. Ne pas laisser de faux « manquants ».
                    if state == VisitState::Saved && !record.name.is_empty() {
                        let key = duplicates::normalize_item(&record.name);
                        let mut fixed = Vec::new();
                        for old in self.visits.iter_mut() {
                            if old.id != id
                                && old.state == VisitState::NeedsReview
                                && !old.name.is_empty()
                                && duplicates::normalize_item(&old.name) == key
                            {
                                old.state = VisitState::Saved;
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
                    if state == VisitState::NeedsReview
                        && state_changed
                        && self.settings.alert_sound
                    {
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
                }
                WorkerEvent::Evidence { id, path } => {
                    if self.dismissed_visit_ids.contains(&id) {
                        continue;
                    }
                    if let Some(v) = self.visits.iter_mut().find(|v| v.id == id) {
                        if v.state == VisitState::NeedsReview && v.evidence.is_empty() {
                            v.evidence = path.to_string_lossy().to_string();
                        }
                    }
                }
                WorkerEvent::Saved { path, note } => {
                    self.invalidate_thumbnails();
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
}
