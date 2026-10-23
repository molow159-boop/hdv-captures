use crate::duplicates;
use crate::ocr::{self, Name};
use crate::settings::{CaptureRect, Settings};
use crate::vision::{self, Fingerprint};
use crate::{diagnostics, index};
use chrono::Local;
use image::RgbaImage;
use screenshots::Screen;
use std::sync::atomic::AtomicI64;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender, TrySendError},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

pub enum WorkerCommand {
    Configure(Settings),
    SetAutomatic(bool),
    Manual,
    Calibrate,
    Diagnostic,
    Shutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureState {
    Ready,
    Waiting,
    Incomplete,
    Captured,
}

pub enum WorkerEvent {
    Saved {
        path: PathBuf,
        note: String,
    },
    State(CaptureState, String),
    Error(String),
    Calibration(RgbaImage),
    Diagnostic(PathBuf),
    Skipped,
    Visit {
        id: i64,
        name: Option<String>,
        state: &'static str,
        reason: String,
        evidence: Option<PathBuf>,
    },
    Evidence {
        id: i64,
        path: PathBuf,
    },
}

pub fn monitors() -> Vec<String> {
    match Screen::all() {
        Ok(screens) => screens
            .iter()
            .enumerate()
            .map(|(i, scr)| {
                format!(
                    "Écran {} — {} × {}",
                    i + 1,
                    scr.display_info.width,
                    scr.display_info.height
                )
            })
            .collect(),
        Err(_) => vec!["Écran indisponible".into()],
    }
}

fn grab(index: usize) -> Result<RgbaImage, String> {
    let screens = Screen::all().map_err(|e| format!("Accès écrans : {e}"))?;
    let screen = screens.get(index).ok_or("Écran choisi introuvable")?;
    let img = screen
        .capture()
        .map_err(|e| format!("Capture écran : {e}"))?;
    RgbaImage::from_raw(img.width(), img.height(), img.into_raw())
        .ok_or("Format d'image inattendu".into())
}

// Windows interdit certains caractères, noms spéciaux et un point final.
// N'utiliser JAMAIS du texte OCR comme un chemin sans le nettoyer.
fn safe_item_filename(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_control() || "<>:\"/\\|?*".contains(c) {
            s.push('_');
        } else if c.is_whitespace() {
            s.push(' ');
        } else {
            s.push(c);
        }
    }
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s
        .trim_matches(|c: char| c == '.' || c == ' ')
        .chars()
        .take(90)
        .collect::<String>();
    let s = s.trim_end_matches(['.', ' ']).to_string();
    if s.is_empty() {
        return "Nom_non_reconnu".into();
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    if [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ]
    .contains(&stem.as_str())
    {
        format!("Objet_{s}")
    } else {
        s
    }
}

/// Un seul PNG par nom normalisé. Les captures automatiques suivantes sont
/// ignorées, même si les pixels fluctuent; le bouton manuel peut actualiser.
enum SaveOutcome {
    Saved(PathBuf),
    AlreadyPresent,
}

fn save(
    img: &RgbaImage,
    destination: &Path,
    name: &str,
    manual: bool,
    quality: &str,
) -> Result<SaveOutcome, String> {
    let _guard = duplicates::lock_capture_files()?;
    std::fs::create_dir_all(destination).map_err(|e| format!("Création dossier : {e}"))?;
    let key = duplicates::normalize_item(name);
    if key.is_empty() {
        return Err("Nom d'objet OCR invalide".into());
    }
    let existing = duplicates::paths_for_item(destination, &key)?;
    if !manual && (!existing.is_empty() || index::item_prepared(destination, &key)?) {
        return Ok(SaveOutcome::AlreadyPresent);
    }
    let target = destination.join(format!("{}.png", safe_item_filename(name)));
    // Ne jamais écraser un fichier au nom identique qui désigne un autre objet.
    if target.exists() && duplicates::item_from_filename(&target).as_deref() != Some(key.as_str()) {
        return Err(format!("Conflit de nom de fichier : {}", target.display()));
    }
    if !manual && target.exists() {
        return Ok(SaveOutcome::AlreadyPresent);
    }
    // Toujours terminer l'écriture avant de toucher à l'ancienne capture.
    let temp = destination.join(format!(
        ".hdv-partial-{}-{}.png",
        std::process::id(),
        Local::now().format("%Y%m%d%H%M%S%9f")
    ));
    img.save_with_format(&temp, image::ImageFormat::Png)
        .map_err(|e| format!("Enregistrement PNG : {e}"))?;
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    if manual && !existing.is_empty() {
        let backup = destination.join("Captures_remplacees");
        if let Err(e) = std::fs::create_dir_all(&backup) {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("Dossier d'archives : {e}"));
        }
        for old in &existing {
            let Some(file_name) = old.file_name() else {
                continue;
            };
            let mut path = backup.join(file_name);
            let mut i = 2;
            while path.exists() {
                path = backup.join(format!("{i}_{}", file_name.to_string_lossy()));
                i += 1;
            }
            if let Err(e) = std::fs::rename(old, &path) {
                for (orig, arch) in moved.iter().rev() {
                    let _ = std::fs::rename(arch, orig);
                }
                let _ = std::fs::remove_file(&temp);
                return Err(format!("Archivage avant actualisation : {e}"));
            }
            moved.push((old.clone(), path));
        }
    }
    match std::fs::rename(&temp, &target) {
        Ok(()) => {
            if let Err(e) = index::saved(destination, &target, name, quality) {
                diagnostics::write("INDEX", &e);
            }
            Ok(SaveOutcome::Saved(target))
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp);
            for (orig, arch) in moved.iter().rev() {
                let _ = std::fs::rename(arch, orig);
            }
            Err(format!("Finalisation de la capture : {e}"))
        }
    }
}

fn locate_panel(frame: &RgbaImage, settings: &Settings) -> Option<CaptureRect> {
    let desired = settings
        .region
        .unwrap_or_else(|| vision::default_region(frame.width(), frame.height()));
    vision::detect(frame, desired).map(|d| d.rect)
}

fn event_state(tx: &Sender<WorkerEvent>, state: CaptureState, message: impl Into<String>) {
    let _ = tx.send(WorkerEvent::State(state, message.into()));
}

struct Job {
    visit_id: i64,
    frame: RgbaImage,
    rect: CaptureRect,
    settings: Settings,
    exe: Option<PathBuf>,
    manual: bool,
    generation: u64,
}

// Le thread capture l'écran rapidement; ce thread séparé effectue l'OCR, le
// PNG et les vérifications coûteuses sans interrompre la surveillance Dofus.
fn processor(
    rx: Receiver<Job>,
    manual_rx: Receiver<Job>,
    tx: Sender<WorkerEvent>,
    current_generation: Arc<AtomicU64>,
    hover_pending: Arc<AtomicBool>,
    retry_rejected: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let mut last_skipped_key: Option<String> = None;
        loop {
            // Priorité à F9 et au bouton manuel. Ces demandes ne doivent pas
            // attendre derrière les refus/reprises du mode automatique.
            let job = match manual_rx.try_recv() {
                Ok(job) => job,
                Err(_) => match rx.recv_timeout(Duration::from_millis(75)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                },
            };
            if job.generation != current_generation.load(Ordering::Relaxed) {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    "a_reprendre",
                    "Analyse interrompue par changement de réglage",
                );
                continue;
            }
            let Some(panel) = vision::crop(&job.frame, job.rect) else {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    "a_reprendre",
                    "Recadrage du panneau impossible",
                );
                retry_rejected.store(true, Ordering::Relaxed);
                continue;
            };
            if !vision::verified_detail_crop(&panel) {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    "a_reprendre",
                    "Structure de fiche invalide",
                );
                retry_rejected.store(true, Ordering::Relaxed);
                event_state(
                    &tx,
                    CaptureState::Incomplete,
                    "Liste HDV ou fiche tronquée : capture refusée (pas de PNG).",
                );
                continue;
            }
            if !vision::offers_still_visible(&job.frame, job.rect) {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    "a_reprendre",
                    "Dernier prix non confirmé",
                );
                retry_rejected.store(true, Ordering::Relaxed);
                if job.manual {
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        "Fiche incomplète : dernier prix non confirmé.",
                    );
                }
                continue;
            }
            let Some(exe) = job.exe.as_deref() else {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    "a_reprendre",
                    "Tesseract indisponible",
                );
                retry_rejected.store(true, Ordering::Relaxed);
                // Même en manuel, aucun fichier anonyme : le nom OCR fait partie
                // du contrat de sauvegarde. Il faut installer Tesseract fra+eng.
                event_state(
                    &tx,
                    CaptureState::Incomplete,
                    if job.manual {
                        "Capture manuelle refusée : Tesseract (fra+eng) nécessaire pour nommer le PNG."
                    } else {
                        "Tesseract OCR requis en mode ON (fra + eng)."
                    },
                );
                continue;
            };
            let title = match ocr::analyze_panel(&job.frame, job.rect, exe) {
                Ok(Some(name)) => name,
                Ok(None) => {
                    visit_with_preview(
                        &tx,
                        &job,
                        &panel,
                        None,
                        "a_reprendre",
                        "Nom illisible par OCR",
                    );
                    retry_rejected.store(true, Ordering::Relaxed);
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        if job.manual {
                            "Capture F9 refusée : nom OCR illisible. Consulte « Diagnostics »."
                        } else {
                            "OCR : nom non reconnu. Nouvelle tentative automatique prévue."
                        },
                    );
                    continue;
                }
                Err(e) => {
                    visit_with_preview(
                        &tx,
                        &job,
                        &panel,
                        None,
                        "a_reprendre",
                        format!("OCR : {e}"),
                    );
                    retry_rejected.store(true, Ordering::Relaxed);
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        format!("Lecture du panneau : {e}"),
                    );
                    continue;
                }
            };
            let (image, name, note) = match title {
                Name::Complete(name) => {
                    hover_pending.store(false, Ordering::Relaxed);
                    (panel, name, "Fiche complète, prix visibles".to_owned())
                }
                Name::Truncated(prefix) => {
                    match ocr::find_hover(&job.frame, job.rect, &prefix, exe) {
                        Ok(Some(hover)) if vision::offers_still_visible(&job.frame, job.rect) => {
                            hover_pending.store(false, Ordering::Relaxed);
                            let expanded = ocr::expanded_rect(
                                job.rect,
                                hover.text_rect,
                                job.frame.width(),
                                job.frame.height(),
                            );
                            let Some(image) = vision::crop(&job.frame, expanded) else {
                                visit(
                                    &tx,
                                    job.visit_id,
                                    Some(&prefix),
                                    "a_reprendre",
                                    "Recadrage infobulle impossible",
                                );
                                continue;
                            };
                            (
                                image,
                                hover.name,
                                "Infobulle avec nom complet, prix visibles".to_owned(),
                            )
                        }
                        Ok(_) => {
                            visit_with_preview(
                                &tx,
                                &job,
                                &panel,
                                Some(&prefix),
                                "en_attente",
                                "Survol requis pour le nom complet",
                            );
                            retry_rejected.store(true, Ordering::Relaxed);
                            hover_pending.store(true, Ordering::Relaxed);
                            event_state(&tx,CaptureState::Waiting,"Nom tronqué : maintiens l'infobulle visible puis appuie à nouveau sur le raccourci si nécessaire.");
                            continue;
                        }
                        Err(e) => {
                            visit(
                                &tx,
                                job.visit_id,
                                Some(&prefix),
                                "a_reprendre",
                                format!("Infobulle : {e}"),
                            );
                            retry_rejected.store(true, Ordering::Relaxed);
                            event_state(
                                &tx,
                                CaptureState::Incomplete,
                                format!("OCR infobulle : {e}"),
                            );
                            continue;
                        }
                    }
                }
                Name::Unreadable => {
                    visit_with_preview(&tx, &job, &panel, None, "a_reprendre", "Nom non reconnu");
                    retry_rejected.store(true, Ordering::Relaxed);
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        "Nom illisible : capture non enregistrée.",
                    );
                    continue;
                }
            };
            if job.generation != current_generation.load(Ordering::Relaxed) {
                visit(
                    &tx,
                    job.visit_id,
                    Some(&name),
                    "a_reprendre",
                    "Capture annulée après changement de réglage",
                );
                continue;
            }
            let key = duplicates::normalize_item(&name);
            match save(&image, &job.settings.destination, &name, job.manual, &note) {
                Ok(SaveOutcome::Saved(path)) => {
                    visit(
                        &tx,
                        job.visit_id,
                        Some(&name),
                        "enregistre",
                        "PNG enregistré",
                    );
                    retry_rejected.store(false, Ordering::Relaxed);
                    last_skipped_key = None;
                    let _ = tx.send(WorkerEvent::Saved {
                        path,
                        note: format!("{name} — {note}"),
                    });
                    event_state(&tx, CaptureState::Captured, format!("Capturé : {name}"));
                }
                Ok(SaveOutcome::AlreadyPresent) => {
                    visit(
                        &tx,
                        job.visit_id,
                        Some(&name),
                        "enregistre",
                        "Déjà enregistré dans la collection",
                    );
                    retry_rejected.store(false, Ordering::Relaxed);
                    if last_skipped_key.as_deref() != Some(key.as_str()) {
                        event_state(
                            &tx,
                            CaptureState::Ready,
                            format!("Déjà capturé : {name} (ignoré)."),
                        );
                        last_skipped_key = Some(key);
                        let _ = tx.send(WorkerEvent::Skipped);
                    }
                }
                Err(e) => {
                    visit(
                        &tx,
                        job.visit_id,
                        Some(&name),
                        "a_reprendre",
                        format!("Enregistrement : {e}"),
                    );
                    retry_rejected.store(true, Ordering::Relaxed);
                    let _ = tx.send(WorkerEvent::Error(e));
                }
            }
        }
    });
}

fn save_visual_diagnostic(
    frame: &RgbaImage,
    found: Option<CaptureRect>,
    settings: &Settings,
) -> Result<PathBuf, String> {
    let (sw, sh) = frame.dimensions();
    let target = found
        .or(settings.region)
        .unwrap_or_else(|| vision::default_region(sw, sh));
    let x = target.x.saturating_sub(120);
    let y = target.y.saturating_sub(120);
    let w = target
        .x
        .saturating_add(target.width)
        .saturating_add(120)
        .min(sw)
        .saturating_sub(x);
    let h = target
        .y
        .saturating_add(target.height)
        .saturating_add(120)
        .min(sh)
        .saturating_sub(y);
    let mut img = vision::crop(
        frame,
        CaptureRect {
            x,
            y,
            width: w,
            height: h,
        },
    )
    .ok_or("Recadrage diagnostic impossible")?;
    if let Some(rect) = found {
        let l = rect.x.saturating_sub(x).min(img.width() - 1);
        let t = rect.y.saturating_sub(y).min(img.height() - 1);
        let r = rect
            .x
            .saturating_add(rect.width)
            .saturating_sub(x)
            .min(img.width() - 1);
        let b = rect
            .y
            .saturating_add(rect.height)
            .saturating_sub(y)
            .min(img.height() - 1);
        for col in l..=r {
            img.put_pixel(col, t, image::Rgba([0, 255, 180, 255]));
            img.put_pixel(col, b, image::Rgba([0, 255, 180, 255]));
        }
        for row in t..=b {
            img.put_pixel(l, row, image::Rgba([0, 255, 180, 255]));
            img.put_pixel(r, row, image::Rgba([0, 255, 180, 255]));
        }
    }
    let folder = crate::settings::config_path().with_file_name("Diagnostics_captures");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let path = folder.join(format!(
        "Capture_diagnostic_{}_{}.png",
        Local::now().format("%Y%m%d_%H%M%S"),
        std::process::id()
    ));
    img.save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    diagnostics::write(
        "DETECTION",
        if found.is_some() {
            "Fiche reconnue (contour vert)"
        } else {
            "Fiche non reconnue"
        },
    );
    Ok(path)
}

struct Candidate {
    fp: Fingerprint,
    visit_id: i64,
    queued: bool,
    preview: Option<RgbaImage>,
    overload_reported: bool,
}

struct EvidenceJob {
    id: i64,
    folder: PathBuf,
    preview: RgbaImage,
}
fn evidence_writer(rx: Receiver<EvidenceJob>, tx: Sender<WorkerEvent>) {
    thread::spawn(move || {
        while let Ok(job) = rx.recv() {
            if std::fs::create_dir_all(&job.folder).is_err() {
                continue;
            }
            let path = job.folder.join(format!("Fiche_{}.png", job.id));
            if job
                .preview
                .save_with_format(&path, image::ImageFormat::Png)
                .is_ok()
            {
                let _ = tx.send(WorkerEvent::Evidence { id: job.id, path });
            }
        }
    });
}
fn mark_skipped(
    tx: &Sender<WorkerEvent>,
    evidence_tx: &SyncSender<EvidenceJob>,
    old: Candidate,
    destination: &Path,
    reason: &str,
) {
    if old.queued {
        return;
    }
    visit(tx, old.visit_id, None, "a_reprendre", reason);
    if let Some(preview) = old.preview {
        // La sauvegarde PNG est asynchrone : ne bloque pas le balayage du jeu.
        let _ = evidence_tx.try_send(EvidenceJob {
            id: old.visit_id,
            folder: destination.join("Fiches_a_reprendre"),
            preview,
        });
    }
}

static VISIT_SEQ: AtomicI64 = AtomicI64::new(0);
fn next_visit_id() -> i64 {
    Local::now()
        .timestamp_micros()
        .saturating_add(VISIT_SEQ.fetch_add(1, Ordering::Relaxed))
}
fn visit(
    tx: &Sender<WorkerEvent>,
    id: i64,
    name: Option<&str>,
    state: &'static str,
    reason: impl Into<String>,
) {
    let _ = tx.send(WorkerEvent::Visit {
        id,
        name: name.map(str::to_owned),
        state,
        reason: reason.into(),
        evidence: None,
    });
}
/// Capture de secours locale destinée au diagnostic, jamais au dossier des PNG
/// validés ni à la préparation d'envoi. Un refus OCR reste ainsi identifiable après
/// changement d'objet. Une image par tentative suffit.
fn visit_with_preview(
    tx: &Sender<WorkerEvent>,
    job: &Job,
    image: &RgbaImage,
    name: Option<&str>,
    state: &'static str,
    reason: impl Into<String>,
) {
    let folder = job.settings.destination.join("Fiches_a_reprendre");
    let evidence = (|| -> Option<PathBuf> {
        std::fs::create_dir_all(&folder).ok()?;
        let path = folder.join(format!("Fiche_{}.png", job.visit_id));
        if !path.exists() {
            image
                .save_with_format(&path, image::ImageFormat::Png)
                .ok()?;
        }
        Some(path)
    })();
    let _ = tx.send(WorkerEvent::Visit {
        id: job.visit_id,
        name: name.map(str::to_owned),
        state,
        reason: reason.into(),
        evidence,
    });
}

pub fn spawn(mut config: Settings, rx: Receiver<WorkerCommand>, tx: Sender<WorkerEvent>) {
    let (job_tx, job_rx): (SyncSender<Job>, Receiver<Job>) = mpsc::sync_channel(10);
    let (manual_tx, manual_rx): (SyncSender<Job>, Receiver<Job>) = mpsc::sync_channel(2);
    let (evidence_tx, evidence_rx) = mpsc::sync_channel(32);
    evidence_writer(evidence_rx, tx.clone());
    let generation = Arc::new(AtomicU64::new(1));
    let hover_pending = Arc::new(AtomicBool::new(false));
    let retry_rejected = Arc::new(AtomicBool::new(false));
    processor(
        job_rx,
        manual_rx,
        tx.clone(),
        Arc::clone(&generation),
        Arc::clone(&hover_pending),
        Arc::clone(&retry_rejected),
    );
    thread::spawn(move || {
        let mut candidate: Option<Candidate> = None;
        let mut last_sent: Option<(Fingerprint, Instant)> = None;
        let mut no_panel_reported = false;
        let mut last_rect: Option<(CaptureRect, Instant)> = None;
        let mut last_global_search = Instant::now() - Duration::from_secs(10);
        let mut ocr_exe = ocr::locate(config.ocr_exe.as_deref());
        event_state(&tx, CaptureState::Ready, "Prêt. Mode automatique OFF.");
        loop {
            // Interroger rapidement l'affichage lorsque le mode ON est actif.
            // L'OCR et l'enregistrement PNG restent hors de ce thread.
            // Intervalle minimal, pas garantie de 22 captures/secondes.
            let wait = if config.automatic {
                Duration::from_millis(45)
            } else {
                Duration::from_millis(250)
            };
            match rx.recv_timeout(wait) {
                Ok(WorkerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    generation.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                Ok(WorkerCommand::Configure(s)) => {
                    // Une modification de touche/mode d'affichage ne justifie
                    // pas de redémarrer Tesseract ni d'effacer tout le suivi.
                    let ocr_changed = config.ocr_exe != s.ocr_exe;
                    let geometry_changed =
                        config.screen_index != s.screen_index || config.region != s.region;
                    let capture_changed =
                        geometry_changed || ocr_changed || config.destination != s.destination;
                    config = s;
                    // Galerie / son / mode compact ne doivent JAMAIS annuler
                    // les fiches déjà en cours d'OCR.
                    if capture_changed {
                        generation.fetch_add(1, Ordering::Relaxed);
                        candidate = None;
                        last_sent = None;
                        retry_rejected.store(false, Ordering::Relaxed);
                        hover_pending.store(false, Ordering::Relaxed);
                        no_panel_reported = false;
                    }
                    if ocr_changed {
                        ocr_exe = ocr::locate(config.ocr_exe.as_deref());
                    }
                    if geometry_changed {
                        last_rect = None;
                    }
                    event_state(
                        &tx,
                        CaptureState::Ready,
                        if config.automatic {
                            "ON : surveillance en cours."
                        } else {
                            "OFF : raccourci manuel disponible."
                        },
                    );
                }
                Ok(WorkerCommand::SetAutomatic(on)) => {
                    if !on {
                        if let Some(old) = candidate.take() {
                            mark_skipped(
                                &tx,
                                &evidence_tx,
                                old,
                                &config.destination,
                                "Collecte arrêtée avant capture",
                            );
                        }
                    }
                    config.automatic = on;
                    // OFF arrête les nouvelles détections mais laisse finir
                    // les PNG déjà placés dans la file OCR.
                    candidate = None;
                    last_sent = None;
                    retry_rejected.store(false, Ordering::Relaxed);
                    hover_pending.store(false, Ordering::Relaxed);
                    no_panel_reported = false;
                    event_state(
                        &tx,
                        CaptureState::Ready,
                        if on {
                            "ON : surveillance en cours."
                        } else {
                            "OFF : raccourci manuel disponible."
                        },
                    );
                }
                Ok(WorkerCommand::Calibrate) => {
                    thread::sleep(Duration::from_millis(650));
                    match grab(config.screen_index) {
                        Ok(img) => {
                            let _ = tx.send(WorkerEvent::Calibration(img));
                        }
                        Err(e) => {
                            let _ = tx.send(WorkerEvent::Error(e));
                        }
                    }
                }
                Ok(WorkerCommand::Diagnostic) => match grab(config.screen_index) {
                    Ok(frame) => {
                        match save_visual_diagnostic(&frame, locate_panel(&frame, &config), &config)
                        {
                            Ok(p) => {
                                let _ = tx.send(WorkerEvent::Diagnostic(p));
                            }
                            Err(e) => {
                                let _ = tx.send(WorkerEvent::Error(e));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(WorkerEvent::Error(e));
                    }
                },
                Ok(WorkerCommand::Manual) => {
                    let visit_id = next_visit_id();
                    match grab(config.screen_index) {
                        Ok(frame) => {
                            if let Some(rect) = locate_panel(&frame, &config) {
                                visit(
                                    &tx,
                                    visit_id,
                                    None,
                                    "en_attente",
                                    "Capture manuelle F9 à analyser",
                                );
                                let job = Job {
                                    visit_id,
                                    frame,
                                    rect,
                                    settings: config.clone(),
                                    exe: ocr_exe.clone(),
                                    manual: true,
                                    generation: generation.load(Ordering::Relaxed),
                                };
                                if manual_tx.try_send(job).is_err() {
                                    visit(
                                        &tx,
                                        visit_id,
                                        None,
                                        "a_reprendre",
                                        "File manuelle pleine",
                                    );
                                    event_state(
                                        &tx,
                                        CaptureState::Incomplete,
                                        "Trop de captures manuelles en attente : réessaie F9.",
                                    );
                                }
                            } else {
                                event_state(
                                    &tx,
                                    CaptureState::Incomplete,
                                    "Aucune fiche de prix reconnue sur l'écran.",
                                );
                            }
                        }
                        Err(e) => {
                            // Pas de fiche observée : ne pas inventer un objet manqué.
                            let _ = tx.send(WorkerEvent::Error(e));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) if config.automatic => {
                    let frame = match grab(config.screen_index) {
                        Ok(img) => img,
                        Err(e) => {
                            event_state(
                                &tx,
                                CaptureState::Incomplete,
                                format!("Capture écran : {e}"),
                            );
                            continue;
                        }
                    };
                    let hint = last_rect
                        .map(|(r, _)| r)
                        .or(config.region)
                        .unwrap_or_else(|| vision::default_region(frame.width(), frame.height()));
                    // Chemin rapide : réutiliser la dernière géométrie SI la fiche
                    // est encore entière. Évite un balayage à chaque image.
                    // Au moindre doute, le détecteur recherche une nouvelle fiche.
                    let mut rect = last_rect.and_then(|(r, _)| {
                        vision::crop(&frame, r)
                            .filter(vision::verified_detail_crop)
                            .and_then(|_| vision::offers_still_visible(&frame, r).then_some(r))
                    });
                    if rect.is_none() {
                        rect = vision::detect_near(&frame, hint).map(|d| d.rect);
                    }
                    if rect.is_none() && last_global_search.elapsed() >= Duration::from_secs(2) {
                        last_global_search = Instant::now();
                        rect = locate_panel(&frame, &config);
                    }
                    let rect = rect.or_else(|| {
                        // Bordure masquée temporairement par une infobulle.
                        last_rect
                            .filter(|(_, time)| time.elapsed() < Duration::from_secs(3))
                            .and_then(|(r, _)| vision::offers_still_visible(&frame, r).then_some(r))
                    });
                    let Some(rect) = rect else {
                        if let Some(old) = candidate.take() {
                            mark_skipped(
                                &tx,
                                &evidence_tx,
                                old,
                                &config.destination,
                                "Fiche disparue avant capture",
                            );
                        }
                        if !no_panel_reported {
                            event_state(
                                &tx,
                                CaptureState::Ready,
                                "Pas de fiche HDV reconnue. Utilise « Visualiser la détection ».",
                            );
                            no_panel_reported = true;
                        }
                        continue;
                    };
                    no_panel_reported = false;
                    last_rect = Some((rect, Instant::now()));
                    let Some(panel) = vision::crop(&frame, rect) else {
                        continue;
                    };
                    let title_fp = vision::title_fingerprint(&panel);
                    // Une animation dans la liste des prix ne doit pas réinitialiser
                    // la stabilisation de l’objet : comparer seulement le titre.
                    let fp = title_fp.clone();

                    match candidate.as_ref() {
                        Some(prev) if vision::same_title(&prev.fp, &fp) => {}
                        _ => {
                            if let Some(old) = candidate.take() {
                                mark_skipped(
                                    &tx,
                                    &evidence_tx,
                                    old,
                                    &config.destination,
                                    "Objet changé trop rapidement",
                                );
                            }
                            let id = next_visit_id();
                            visit(
                                &tx,
                                id,
                                None,
                                "en_attente",
                                "Fiche ouverte, en cours de reconnaissance",
                            );
                            // Conserver le panneau dès sa première apparition, avant
                            // de déléguer le traitement OCR. Aucun délai fixe.
                            candidate = Some(Candidate {
                                fp: fp.clone(),
                                visit_id: id,
                                queued: false,
                                preview: Some(panel.clone()),
                                overload_reported: false,
                            });
                        }
                    }
                    // Éviter un OCR coûteux en continu sur la même fiche.
                    // En attente d'infobulle, réessayer plus vite.
                    let delay = if hover_pending.load(Ordering::Relaxed) {
                        Duration::from_millis(380)
                    } else if retry_rejected.load(Ordering::Relaxed) {
                        Duration::from_millis(480)
                    } else {
                        Duration::from_secs(8)
                    };
                    if last_sent.as_ref().is_some_and(|(old, t)| {
                        vision::same_title(old, &title_fp) && t.elapsed() < delay
                    }) {
                        continue;
                    }
                    let visit_id = candidate
                        .as_ref()
                        .map(|c| c.visit_id)
                        .unwrap_or_else(next_visit_id);
                    let job = Job {
                        visit_id,
                        frame,
                        rect,
                        settings: config.clone(),
                        exe: ocr_exe.clone(),
                        manual: false,
                        generation: generation.load(Ordering::Relaxed),
                    };
                    match job_tx.try_send(job) {
                        Ok(()) => {
                            if let Some(c) = candidate.as_mut() {
                                c.queued = true;
                                c.preview = None;
                            }
                            last_sent = Some((title_fp, Instant::now()));
                        }
                        Err(TrySendError::Full(_)) => {
                            // Retenter dans la boucle suivante, mais ne pas inonder
                            // l'interface d'alertes identiques toutes les 45 ms.
                            if let Some(c) = candidate.as_mut() {
                                if !c.overload_reported {
                                    c.overload_reported = true;
                                    event_state(&tx,CaptureState::Incomplete,
                                        "OCR en retard : image conservée tant que la fiche reste affichée.");
                                }
                            }
                        }
                        Err(TrySendError::Disconnected(_)) => {
                            let _ = tx.send(WorkerEvent::Error("Thread OCR arrêté".into()));
                            break;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filename_rejects_windows_special_chars() {
        assert_eq!(safe_item_filename("Cape / Glace: 3?"), "Cape _ Glace_ 3_");
        assert_eq!(safe_item_filename("CON"), "Objet_CON");
        assert_eq!(safe_item_filename(".."), "Nom_non_reconnu");
    }
    #[test]
    fn auto_and_manual_use_same_item_filename_and_archive_old_image() {
        let folder =
            std::env::temp_dir().join(format!("hdv-noms-auto-manuel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let a = RgbaImage::from_pixel(250, 230, image::Rgba([2, 3, 4, 255]));
        let b = RgbaImage::from_pixel(250, 230, image::Rgba([9, 8, 7, 255]));
        let name = "Bottes Phossiles";
        let first = match save(&a, &folder, name, false, "test").unwrap() {
            SaveOutcome::Saved(path) => path,
            _ => panic!("first auto capture should save"),
        };
        assert_eq!(first.file_name().unwrap(), "Bottes Phossiles.png");
        assert!(matches!(
            save(&b, &folder, "bottes phossiles", false, "test").unwrap(),
            SaveOutcome::AlreadyPresent
        ));
        assert_eq!(image::open(&first).unwrap().to_rgba8(), a);
        let refreshed = match save(&b, &folder, name, true, "test").unwrap() {
            SaveOutcome::Saved(path) => path,
            _ => panic!("manual capture should refresh same named file"),
        };
        assert_eq!(first, refreshed);
        assert_eq!(image::open(&refreshed).unwrap().to_rgba8(), b);
        let archive = folder.join("Captures_remplacees");
        let archives: Vec<_> = std::fs::read_dir(&archive).unwrap().collect();
        assert_eq!(archives.len(), 1);
        assert_eq!(
            image::open(archives[0].as_ref().unwrap().path())
                .unwrap()
                .to_rgba8(),
            a
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
    #[test]
    fn accented_item_keeps_readable_filename() {
        assert_eq!(safe_item_filename("Épée du héros"), "Épée du héros");
        assert_eq!(safe_item_filename("AUX"), "Objet_AUX");
    }
    #[test]
    fn prepared_capture_is_not_recreated_automatically_but_manual_refresh_remains_possible() {
        let folder = std::env::temp_dir().join(format!(
            "hdv-prepared-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap()
        ));
        let image = RgbaImage::from_pixel(250, 230, image::Rgba([2, 3, 4, 255]));
        assert!(matches!(
            save(&image, &folder, "Objet Test", false, "test").unwrap(),
            SaveOutcome::Saved(_)
        ));
        let batch = index::prepare(&folder, None).unwrap();
        assert_eq!(batch.moved, 1);
        assert!(matches!(
            save(&image, &folder, "Objet Test", false, "test").unwrap(),
            SaveOutcome::AlreadyPresent
        ));
        assert!(!folder.join("Objet Test.png").exists());
        assert!(matches!(
            save(&image, &folder, "Objet Test", true, "test").unwrap(),
            SaveOutcome::Saved(_)
        ));
        assert_eq!(index::prepare(&folder, None).unwrap().moved, 1);
        assert!(batch.path.join("Objet Test.png").is_file());
    }
}
