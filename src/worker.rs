use crate::duplicates;
use crate::error::AppError;
use crate::ocr::{self, Name};
use crate::settings::{CaptureRect, Settings};
use crate::state::{ProcessingPhase, VisitState};
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
    Pause(Sender<()>),
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
    Scoped {
        folder: PathBuf,
        session: String,
        event: Box<WorkerEvent>,
    },
    Timing {
        waiting: Duration,
        ocr: Duration,
        save: Duration,
    },
    Progress {
        phase: ProcessingPhase,
        elapsed: Duration,
    },
    Saved {
        path: PathBuf,
        note: String,
    },
    State(CaptureState, String),
    Error(AppError),
    Calibration(RgbaImage),
    Diagnostic(PathBuf),
    Skipped,
    Visit {
        id: i64,
        name: Option<String>,
        state: VisitState,
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

fn grab(index: usize) -> Result<RgbaImage, AppError> {
    thread_local! {static SCREENS:std::cell::RefCell<Option<(Instant,Vec<Screen>)>>=const{std::cell::RefCell::new(None)};}
    let screen = SCREENS.with(|slot| -> Result<Screen, AppError> {
        let mut cached = slot.borrow_mut();
        if !cached
            .as_ref()
            .is_some_and(|(time, _)| time.elapsed() < Duration::from_secs(2))
        {
            *cached = Some((
                Instant::now(),
                Screen::all().map_err(|e| AppError::context("Accès écrans", e))?,
            ));
        }
        cached
            .as_ref()
            .and_then(|(_, screens)| screens.get(index))
            .copied()
            .ok_or_else(|| AppError::Domain(crate::error::DomainError::ScreenNotFound))
    })?;
    let img = screen
        .capture()
        .map_err(|e| AppError::context("Capture écran", e))?;
    RgbaImage::from_raw(img.width(), img.height(), img.into_raw())
        .ok_or(AppError::Domain(crate::error::DomainError::UnexpectedImage))
}

// Windows interdit certains caractères, noms spéciaux et un point final.
// N'utiliser JAMAIS du texte OCR comme un chemin sans le nettoyer.
pub(crate) fn safe_item_filename(name: &str) -> String {
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
) -> Result<SaveOutcome, AppError> {
    let _guard = duplicates::lock_capture_files()?;
    std::fs::create_dir_all(destination).map_err(|e| AppError::context("Création dossier", e))?;
    let key = duplicates::normalize_item(name);
    if key.is_empty() {
        return Err(AppError::Domain(crate::error::DomainError::InvalidOcrName));
    }
    let existing = duplicates::paths_for_item(destination, &key)?;
    if !manual && (!existing.is_empty() || index::item_prepared(destination, &key)?) {
        return Ok(SaveOutcome::AlreadyPresent);
    }
    let target = destination.join(format!("{}.png", safe_item_filename(name)));
    // Ne jamais écraser un fichier au nom identique qui désigne un autre objet.
    if target.exists() && duplicates::item_from_filename(&target).as_deref() != Some(key.as_str()) {
        return Err(AppError::FileConflict(target.clone()));
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
        .map_err(|e| AppError::context("Enregistrement PNG", e))?;
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    if manual && !existing.is_empty() {
        let backup = destination.join("Captures_remplacees");
        duplicates::ensure_plain_child(destination, &backup)?;
        if let Err(e) = std::fs::create_dir_all(&backup) {
            let _ = std::fs::remove_file(&temp);
            return Err(AppError::context("Dossier d'archives", e));
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
                return Err(AppError::context("Archivage avant actualisation", e));
            }
            moved.push((old.clone(), path));
        }
    }
    match std::fs::rename(&temp, &target) {
        Ok(()) => {
            duplicates::saved(destination, &target, name);
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
            Err(AppError::context("Finalisation de la capture", e))
        }
    }
}

fn locate_panel(frame: &RgbaImage, settings: &Settings) -> Option<CaptureRect> {
    let desired = settings
        .region
        .unwrap_or_else(|| vision::default_region(frame.width(), frame.height()));
    vision::detect(frame, desired).map(|d| d.rect)
}

fn event_state(tx: &EventSender, state: CaptureState, message: impl Into<String>) {
    let _ = tx.send(WorkerEvent::State(state, message.into()));
}

#[derive(Clone)]
struct EventSender {
    sender: Sender<WorkerEvent>,
    folder: PathBuf,
    session: String,
}
impl EventSender {
    fn at(&self, settings: &Settings) -> Self {
        Self {
            sender: self.sender.clone(),
            folder: settings.destination.clone(),
            session: settings.runtime_session.clone(),
        }
    }
    fn send(&self, event: WorkerEvent) -> Result<(), ()> {
        self.sender
            .send(WorkerEvent::Scoped {
                folder: self.folder.clone(),
                session: self.session.clone(),
                event: Box::new(event),
            })
            .map_err(|_| ())
    }
}
pub struct Runtime {
    pub pending: Arc<std::sync::atomic::AtomicUsize>,
    pub admitted: Arc<std::sync::atomic::AtomicBool>,
    pub finished: Arc<std::sync::atomic::AtomicBool>,
}
struct JobCompletion {
    pending: Arc<std::sync::atomic::AtomicUsize>,
    sender: EventSender,
    started: Instant,
    processing: bool,
    waiting: Duration,
    ocr: Duration,
    save: Duration,
}
impl Drop for JobCompletion {
    fn drop(&mut self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
        if !self.processing {
            return;
        }
        let _ = self.sender.send(WorkerEvent::Timing {
            waiting: self.waiting,
            ocr: self.ocr,
            save: self.save,
        });
        let _ = self.sender.send(WorkerEvent::Progress {
            phase: ProcessingPhase::Finished,
            elapsed: self.started.elapsed(),
        });
    }
}
fn compact_job(frame: RgbaImage, rect: CaptureRect) -> (RgbaImage, CaptureRect) {
    let left = rect.x.saturating_sub(240);
    let top = rect.y.saturating_sub(130);
    let right = rect
        .x
        .saturating_add(rect.width)
        .saturating_add(240)
        .min(frame.width());
    let bottom = rect
        .y
        .saturating_add(rect.height)
        .max(rect.y.saturating_add(265))
        .min(frame.height());
    let image = image::imageops::crop_imm(&frame, left, top, right - left, bottom - top).to_image();
    (
        image,
        CaptureRect {
            x: rect.x - left,
            y: rect.y - top,
            ..rect
        },
    )
}
struct Job {
    visit_id: i64,
    frame: RgbaImage,
    rect: CaptureRect,
    settings: Settings,
    exe: Option<PathBuf>,
    manual: bool,
    generation: u64,
    completion: JobCompletion,
}

// Le thread capture l'écran rapidement; ce thread séparé effectue l'OCR, le
// PNG et les vérifications coûteuses sans interrompre la surveillance Dofus.
fn processor(
    rx: Receiver<Job>,
    manual_rx: Receiver<Job>,
    tx: EventSender,
    current_generation: Arc<AtomicU64>,
    hover_pending: Arc<AtomicBool>,
    retry_rejected: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_skipped_key: Option<String> = None;
        loop {
            // Priorité à raccourci manuel et au bouton manuel. Ces demandes ne doivent pas
            // attendre derrière les refus/reprises du mode automatique.
            let mut job = match manual_rx.try_recv() {
                Ok(job) => job,
                Err(_) => match rx.recv_timeout(Duration::from_millis(75)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                },
            };
            job.completion.processing = true;
            job.completion.waiting = job.completion.started.elapsed();
            job.completion.started = Instant::now();
            let tx = tx.at(&job.settings);
            let set_retry = |value| {
                if job.generation == current_generation.load(Ordering::Relaxed) {
                    retry_rejected.store(value, Ordering::Relaxed);
                }
            };
            let set_hover = |value| {
                if job.generation == current_generation.load(Ordering::Relaxed) {
                    hover_pending.store(value, Ordering::Relaxed);
                }
            };
            let _ = tx.send(WorkerEvent::Progress {
                phase: ProcessingPhase::Recognizing,
                elapsed: Duration::ZERO,
            });
            let _ = &job.completion;

            let Some(panel) = vision::crop(&job.frame, job.rect) else {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    VisitState::NeedsReview,
                    "Recadrage du panneau impossible",
                );
                set_retry(true);
                continue;
            };
            if !vision::verified_detail_crop(&panel) {
                visit(
                    &tx,
                    job.visit_id,
                    None,
                    VisitState::NeedsReview,
                    "Structure de fiche invalide",
                );
                set_retry(true);
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
                    VisitState::NeedsReview,
                    "Dernier prix non confirmé",
                );
                set_retry(true);
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
                    VisitState::NeedsReview,
                    "Tesseract indisponible",
                );
                set_retry(true);
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
            let ocr_start = Instant::now();
            let recognition = ocr::analyze_panel(&job.frame, job.rect, exe);
            job.completion.ocr += ocr_start.elapsed();
            let title = match recognition {
                Ok(Some(name)) => name,
                Ok(None) => {
                    visit_with_preview(
                        &tx,
                        &job,
                        &panel,
                        None,
                        VisitState::NeedsReview,
                        "Nom illisible par OCR",
                    );
                    set_retry(true);
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        if job.manual {
                            "Capture manuelle refusée : nom OCR illisible. Consulte « Diagnostics »."
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
                        VisitState::NeedsReview,
                        format!("OCR : {e}"),
                    );
                    set_retry(true);
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
                    set_hover(false);
                    (panel, name, "Fiche complète, prix visibles".to_owned())
                }
                Name::Truncated(prefix) => {
                    let ocr_start = Instant::now();
                    let hover = ocr::find_hover(&job.frame, job.rect, &prefix, exe);
                    job.completion.ocr += ocr_start.elapsed();
                    match hover {
                        Ok(Some(hover)) if vision::offers_still_visible(&job.frame, job.rect) => {
                            set_hover(false);
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
                                    VisitState::NeedsReview,
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
                                VisitState::Pending,
                                "Survol requis pour le nom complet",
                            );
                            set_retry(true);
                            set_hover(true);
                            event_state(&tx,CaptureState::Waiting,"Nom tronqué : maintiens l'infobulle visible puis appuie à nouveau sur le raccourci si nécessaire.");
                            continue;
                        }
                        Err(e) => {
                            visit(
                                &tx,
                                job.visit_id,
                                Some(&prefix),
                                VisitState::NeedsReview,
                                format!("Infobulle : {e}"),
                            );
                            set_retry(true);
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
                    visit_with_preview(
                        &tx,
                        &job,
                        &panel,
                        None,
                        VisitState::NeedsReview,
                        "Nom non reconnu",
                    );
                    set_retry(true);
                    event_state(
                        &tx,
                        CaptureState::Incomplete,
                        "Nom illisible : capture non enregistrée.",
                    );
                    continue;
                }
            };

            let key = duplicates::normalize_item(&name);
            let save_start = Instant::now();
            let outcome = save(&image, &job.settings.destination, &name, job.manual, &note);
            job.completion.save += save_start.elapsed();
            match outcome {
                Ok(SaveOutcome::Saved(path)) => {
                    visit(
                        &tx,
                        job.visit_id,
                        Some(&name),
                        VisitState::Saved,
                        "PNG enregistré",
                    );
                    set_retry(false);
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
                        VisitState::Saved,
                        "Déjà enregistré dans la collection",
                    );
                    set_retry(false);
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
                        VisitState::NeedsReview,
                        format!("Enregistrement : {e}"),
                    );
                    set_retry(true);
                    let _ = tx.send(WorkerEvent::Error(e));
                }
            }
        }
    })
}

fn save_visual_diagnostic(
    frame: &RgbaImage,
    found: Option<CaptureRect>,
    settings: &Settings,
) -> Result<PathBuf, AppError> {
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
    .ok_or(AppError::Domain(crate::error::DomainError::DiagnosticCrop))?;
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
    std::fs::create_dir_all(&folder).map_err(AppError::from)?;
    let path = folder.join(format!(
        "Capture_diagnostic_{}_{}.png",
        Local::now().format("%Y%m%d_%H%M%S"),
        std::process::id()
    ));
    img.save_with_format(&path, image::ImageFormat::Png)
        .map_err(AppError::from)?;
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
    sender: EventSender,
}
fn evidence_writer(rx: Receiver<EvidenceJob>, _tx: EventSender) -> std::thread::JoinHandle<()> {
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
                let _ = job.sender.send(WorkerEvent::Evidence { id: job.id, path });
            }
        }
    })
}

fn mark_skipped(
    tx: &EventSender,
    evidence_tx: &SyncSender<EvidenceJob>,
    old: Candidate,
    destination: &Path,
    reason: &str,
) {
    if old.queued {
        return;
    }
    visit(tx, old.visit_id, None, VisitState::NeedsReview, reason);
    if let Some(preview) = old.preview {
        // La sauvegarde PNG est asynchrone : ne bloque pas le balayage du jeu.
        let _ = evidence_tx.try_send(EvidenceJob {
            id: old.visit_id,
            folder: destination.join("Fiches_a_reprendre"),
            preview,
            sender: tx.clone(),
        });
    }
}

static VISIT_SEQ: AtomicI64 = AtomicI64::new(0);
fn next_visit_id() -> i64 {
    let now = Local::now().timestamp_micros();
    let mut previous = VISIT_SEQ.load(Ordering::Relaxed);
    loop {
        let next = now.max(previous.saturating_add(1));
        match VISIT_SEQ.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return next,
            Err(actual) => previous = actual,
        }
    }
}

fn visit(
    tx: &EventSender,
    id: i64,
    name: Option<&str>,
    state: VisitState,
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
    tx: &EventSender,
    job: &Job,
    image: &RgbaImage,
    name: Option<&str>,
    state: VisitState,
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

pub fn spawn(
    mut config: Settings,
    rx: Receiver<WorkerCommand>,
    sender: Sender<WorkerEvent>,
) -> Runtime {
    let tx = EventSender {
        sender,
        folder: config.destination.clone(),
        session: config.runtime_session.clone(),
    };
    let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let admitted = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let runtime = Runtime {
        pending: pending.clone(),
        admitted: admitted.clone(),
        finished: finished.clone(),
    };
    let (job_tx, job_rx): (SyncSender<Job>, Receiver<Job>) = mpsc::sync_channel(4);
    let (manual_tx, manual_rx): (SyncSender<Job>, Receiver<Job>) = mpsc::sync_channel(2);
    let (evidence_tx, evidence_rx) = mpsc::sync_channel(32);
    let evidence_thread = evidence_writer(evidence_rx, tx.clone());
    let generation = Arc::new(AtomicU64::new(1));
    let hover_pending = Arc::new(AtomicBool::new(false));
    let retry_rejected = Arc::new(AtomicBool::new(false));
    let processor_thread = processor(
        job_rx,
        manual_rx,
        tx.clone(),
        Arc::clone(&generation),
        Arc::clone(&hover_pending),
        Arc::clone(&retry_rejected),
    );
    thread::spawn(move || {
        let mut tx = tx;
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
                    admitted.store(false, Ordering::SeqCst);
                    break;
                }
                Ok(WorkerCommand::Configure(s)) => {
                    admitted.store(false, Ordering::SeqCst);
                    // Une modification de touche/mode d'affichage ne justifie
                    // pas de redémarrer Tesseract ni d'effacer tout le suivi.
                    let ocr_changed = config.ocr_exe != s.ocr_exe;
                    let geometry_changed =
                        config.screen_index != s.screen_index || config.region != s.region;
                    let capture_changed =
                        geometry_changed || ocr_changed || config.destination != s.destination;
                    config = s;
                    tx = tx.at(&config);
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
                    admitted.store(on, Ordering::SeqCst);
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
                Ok(WorkerCommand::Pause(ack)) => {
                    config.automatic = false;
                    admitted.store(false, Ordering::SeqCst);
                    let _ = ack.send(());
                }
                Ok(WorkerCommand::Manual) => {
                    let visit_id = next_visit_id();
                    match grab(config.screen_index) {
                        Ok(frame) => {
                            if let Some(rect) = locate_panel(&frame, &config) {
                                visit(
                                    &tx,
                                    visit_id,
                                    None,
                                    VisitState::Pending,
                                    "Capture manuelle à analyser",
                                );
                                let (frame, rect) = compact_job(frame, rect);
                                pending.fetch_add(1, Ordering::SeqCst);
                                let completion = JobCompletion {
                                    pending: pending.clone(),
                                    sender: tx.at(&config),
                                    started: Instant::now(),
                                    processing: false,
                                    waiting: Duration::ZERO,
                                    ocr: Duration::ZERO,
                                    save: Duration::ZERO,
                                };
                                let job = Job {
                                    completion,
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
                                        VisitState::NeedsReview,
                                        "File manuelle pleine",
                                    );
                                    event_state(
                                        &tx,
                                        CaptureState::Incomplete,
                                        "Trop de captures manuelles en attente : réessaie le raccourci manuel.",
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
                                VisitState::Pending,
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
                    let (frame, rect) = compact_job(frame, rect);
                    pending.fetch_add(1, Ordering::SeqCst);
                    let completion = JobCompletion {
                        pending: pending.clone(),
                        sender: tx.at(&config),
                        started: Instant::now(),
                        processing: false,
                        waiting: Duration::ZERO,
                        ocr: Duration::ZERO,
                        save: Duration::ZERO,
                    };
                    let job = Job {
                        completion,
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
                            let _ = tx.send(WorkerEvent::Error(AppError::Domain(
                                crate::error::DomainError::OcrStopped,
                            )));
                            break;
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        drop(job_tx);
        drop(manual_tx);
        drop(evidence_tx);
        let _ = processor_thread.join();
        let _ = evidence_thread.join();
        finished.store(true, Ordering::SeqCst);
    });
    runtime
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_jobs_keep_the_panel_and_tooltip_context_without_the_full_screen() {
        let frame = RgbaImage::from_pixel(3840, 2160, image::Rgba([12, 23, 34, 255]));
        let rect = CaptureRect {
            x: 1500,
            y: 300,
            width: 413,
            height: 700,
        };
        let (compact, local) = compact_job(frame, rect);
        assert!(compact.width() < 1000);
        assert!(compact.height() < 1000);
        assert_eq!(local.width, rect.width);
        assert_eq!(local.height, rect.height);
        assert_eq!(
            vision::crop(&compact, local).unwrap().dimensions(),
            (413, 700)
        );
    }
    #[test]
    fn manual_queue_has_priority_and_completion_is_counted_even_on_rejection() {
        let (tx, events) = mpsc::channel();
        let folder = std::env::temp_dir().join("hdv-queue-test");
        let sender = EventSender {
            sender: tx,
            folder: folder.clone(),
            session: "test".into(),
        };
        let (auto_tx, auto_rx) = mpsc::sync_channel(2);
        let (manual_tx, manual_rx) = mpsc::sync_channel(2);
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let job = |visit_id| Job {
            visit_id,
            frame: RgbaImage::new(1, 1),
            rect: CaptureRect::default(),
            settings: Settings {
                destination: folder.clone(),
                ..Default::default()
            },
            exe: None,
            manual: false,
            generation: 1,
            completion: JobCompletion {
                pending: pending.clone(),
                sender: sender.clone(),
                started: Instant::now(),
                processing: false,
                waiting: Duration::ZERO,
                ocr: Duration::ZERO,
                save: Duration::ZERO,
            },
        };
        auto_tx.send(job(22)).unwrap();
        manual_tx.send(job(11)).unwrap();
        drop(auto_tx);
        drop(manual_tx);
        let worker = processor(
            auto_rx,
            manual_rx,
            sender,
            Arc::new(AtomicU64::new(1)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        worker.join().unwrap();
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        let ids = events
            .try_iter()
            .filter_map(|event| {
                if let WorkerEvent::Scoped { event, .. } = event {
                    if let WorkerEvent::Visit { id, .. } = *event {
                        return Some(id);
                    }
                }
                None
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![11, 22]);
    }
    #[test]
    fn visit_ids_remain_unique_when_called_concurrently() {
        let handles = (0..8)
            .map(|_| std::thread::spawn(|| (0..1000).map(|_| next_visit_id()).collect::<Vec<_>>()))
            .collect::<Vec<_>>();
        let ids = handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 8000);
    }
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
