//! Background gallery scans and export coordination.
use super::*;
impl App {
    pub(super) fn mark_gallery_dirty(&mut self) {
        if !self.gallery_dirty {
            self.gallery_changed_at = Instant::now();
        }
        self.gallery_dirty = true;
    }

    pub(super) fn scan_gallery(&mut self) {
        self.gallery_dirty = true;
        self.gallery_changed_at = Instant::now();
        self.start_gallery_scan();
    }

    pub(super) fn refresh_gallery(&mut self) {
        self.invalidate_thumbnails();
        // Discard a scan started before files were moved, including its preview.
        self.gallery_scan_rx = None;
        self.gallery.clear();
        self.capture_count = None;
        self.thumbs.clear();
        self.selected_path = None;
        self.selected_texture = None;
        self.selected_visit = None;
        self.gallery_filter = GalleryFilter::All;
        self.scan_gallery();
    }

    pub(super) fn refresh_gallery_button(&mut self, ui: &mut egui::Ui) {
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

    pub(super) fn start_gallery_scan(&mut self) {
        if self.tools.history_loading() {
            return;
        }
        if self.gallery_scan_rx.is_some() || !self.gallery_dirty {
            return;
        }
        self.gallery_dirty = false;
        let folder = self.settings.destination.clone();
        let gallery_enabled = self.settings.show_gallery;
        let (tx, rx) = mpsc::channel();
        self.gallery_scan_rx = Some(rx);
        std::thread::spawn(move || {
            let mut errors = Vec::new();
            if gallery_enabled {
                if let Err(e) = index::sync(&folder) {
                    diagnostics::write("INDEX", &e);
                    errors.push(e);
                }
            }
            let (count, mut files) = match duplicates::png_files(&folder) {
                Ok(files) => (Some(files.len()), files),
                Err(error) => {
                    errors.push(error);
                    (None, Vec::new())
                }
            };
            let (stats, prepared) = if gallery_enabled {
                files.sort_by_cached_key(|path| {
                    std::cmp::Reverse(std::fs::metadata(path).and_then(|m| m.modified()).ok())
                });
                (
                    index::stats(&folder).map_err(|e| errors.push(e)).ok(),
                    index::prepared_paths(&folder)
                        .map_err(|e| errors.push(e))
                        .ok(),
                )
            } else {
                files.clear();
                (None, None)
            };
            let _ = tx.send(GallerySnapshot {
                folder,
                count,
                gallery_enabled,
                errors,
                files,
                stats,
                prepared,
            });
        });
    }

    pub(super) fn check_gallery_scan(&mut self) {
        let result = self.gallery_scan_rx.as_ref().map(|rx| rx.try_recv());
        match result {
            Some(Ok(snapshot)) => {
                self.gallery_scan_rx = None;
                if snapshot.folder == self.settings.destination
                    && snapshot.gallery_enabled == self.settings.show_gallery
                {
                    for error in snapshot.errors {
                        self.report_error(error);
                    }
                    self.capture_count = snapshot.count;
                    self.gallery = snapshot.files;
                    self.tools
                        .selected
                        .retain(|path| self.gallery.contains(path));
                    if let Some(stats) = snapshot.stats {
                        self.stats = stats;
                    }
                    if let Some(prepared) = snapshot.prepared {
                        self.prepared_paths = prepared;
                    }
                    let live = self.gallery.iter().collect::<HashSet<_>>();
                    self.thumbs.retain(|path, _| live.contains(path));
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

    pub(super) fn toggle_gallery(&mut self) {
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

    pub(super) fn archive_capture(&mut self, path: &Path) -> Result<(), AppError> {
        self.archive_paths(vec![path.to_path_buf()]);
        Ok(())
    }
    pub(super) fn start_export(&mut self) {
        if self.pipeline_busy() {
            return;
        }
        self.pause_pipeline();
        self.tools.export_waiting = true;
        self.tools.export_selection =
            (!self.tools.selected.is_empty()).then(|| self.tools.selected.clone());
        self.settings.automatic = false;

        self.notice = "Fin des analyses en cours avant préparation…".into();
    }
    pub(super) fn launch_export(&mut self) {
        let folder = self.settings.destination.clone();
        let chunk_size = self
            .settings
            .split_exports
            .then_some(self.settings.export_chunk_size.max(1));
        let (tx, rx) = mpsc::channel();
        self.export_rx = Some(rx);
        self.notice = "Préparation des fichiers en cours…".into();
        let selected = self.tools.export_selection.take();
        std::thread::spawn(move || {
            let result = match selected {
                Some(paths) => index::prepare_selection(&folder, chunk_size, Some(&paths)),
                None => index::prepare(&folder, chunk_size),
            };
            let _ = tx.send(result);
        });
    }
    pub(super) fn check_export(&mut self) {
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
                self.tools.selected.clear();
                self.tools.batches_dirty = true;
                self.refresh_gallery();
            }
            Some(Ok(Err(e))) => {
                self.export_rx = None;
                self.report_error(e);
                self.refresh_gallery();
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.export_rx = None;
                self.report_error("Préparation interrompue");
            }
            _ => {}
        }
    }
}
