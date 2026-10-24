//! Catalogue local SQLite (aucun envoi réseau).
use crate::duplicates;
use crate::error::AppError;
use crate::state::VisitState;
use chrono::Local;
use rusqlite::{params, Connection};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
#[derive(Default, Clone)]
pub struct Stats {
    pub total: usize,
    pub pending: usize,
    pub prepared: usize,
}
fn storage() -> PathBuf {
    #[cfg(not(test))]
    {
        crate::settings::config_path().with_file_name("captures.sqlite3")
    }
    #[cfg(test)]
    {
        // Even `cargo test` on an installed user's machine must never open
        // their real capture database. All writer tests share this fresh file.
        static TEST_DATABASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_DATABASE
            .get_or_init(|| {
                std::env::temp_dir().join(format!(
                    "hdv-index-tests-{}-{}.sqlite3",
                    std::process::id(),
                    Local::now().timestamp_micros()
                ))
            })
            .clone()
    }
}
static DATABASE_INIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static DATABASE_WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Database {
    connection: Connection,
    _guard: std::sync::MutexGuard<'static, ()>,
}
impl std::ops::Deref for Database {
    type Target = Connection;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}
impl std::ops::DerefMut for Database {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}
fn connect() -> Result<Database, AppError> {
    let guard = DATABASE_WRITER
        .lock()
        .map_err(|_| AppError::Domain(crate::error::DomainError::DatabaseUnavailable))?;
    let path = storage();
    if !DATABASE_INIT.load(std::sync::atomic::Ordering::Relaxed) {
        (|| -> Result<(), AppError> {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(AppError::from)?;
            }
            let init = Connection::open(&path).map_err(AppError::from)?;
            init.busy_timeout(Duration::from_secs(1))
                .map_err(AppError::from)?;
            init.execute_batch("PRAGMA journal_mode=WAL;
  CREATE TABLE IF NOT EXISTS items(path TEXT PRIMARY KEY,folder TEXT NOT NULL,item_key TEXT NOT NULL,item_name TEXT NOT NULL,
  captured_at TEXT NOT NULL,prepared_at TEXT,quality TEXT NOT NULL);
  CREATE INDEX IF NOT EXISTS idx_item ON items(folder,item_key);
  CREATE TABLE IF NOT EXISTS export_operations(id TEXT PRIMARY KEY);
  CREATE TABLE IF NOT EXISTS visits(
      id INTEGER PRIMARY KEY, folder TEXT NOT NULL, item_name TEXT NOT NULL DEFAULT '',
      state TEXT NOT NULL, reason TEXT NOT NULL, observed_at TEXT NOT NULL,
      evidence TEXT NOT NULL DEFAULT '', session_id TEXT NOT NULL DEFAULT '');
  CREATE INDEX IF NOT EXISTS idx_visits ON visits(folder,id DESC);")
      .map_err(AppError::from)?;
            // Migration idempotente pour les utilisateurs d'une version de test 0.2.5.
            let _ = init.execute(
                "ALTER TABLE visits ADD COLUMN evidence TEXT NOT NULL DEFAULT ''",
                [],
            );
            let _ = init.execute(
                "ALTER TABLE visits ADD COLUMN session_id TEXT NOT NULL DEFAULT ''",
                [],
            );
            Ok(())
        })()?;
        DATABASE_INIT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Ne jamais bloquer l'interface pendant quatre secondes sur un verrou SQL.
    let conn = Connection::open(&path).map_err(AppError::from)?;
    conn.busy_timeout(Duration::from_secs(3))
        .map_err(AppError::from)?;
    Ok(Database {
        connection: conn,
        _guard: guard,
    })
}
fn folder_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
pub fn sync(folder: &Path) -> Result<(), AppError> {
    let _guard = duplicates::lock_capture_files()?;
    sync_files(folder)
}

fn sync_files(folder: &Path) -> Result<(), AppError> {
    let mut db = connect()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(AppError::from)?;
    for file in duplicates::png_files(folder)? {
        let Some(key) = duplicates::item_from_filename(&file) else {
            continue;
        };
        let label = file.file_stem().unwrap_or_default().to_string_lossy();
        let label = label.split("__").next().unwrap_or(&label).to_owned();
        tx.execute("INSERT OR IGNORE INTO items(path,folder,item_key,item_name,captured_at,quality) VALUES (?1,?2,?3,?4,?5,'historique à vérifier')",
    params![file.to_string_lossy().as_ref(),folder_key(folder),key,label,Local::now().to_rfc3339()])
    .map_err(AppError::from)?;
    }
    let mut stmt = tx
        .prepare("SELECT path FROM items WHERE folder=?1")
        .map_err(AppError::from)?;
    let paths = stmt
        .query_map(params![folder_key(folder)], |r| r.get::<_, String>(0))
        .map_err(AppError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::from)?;
    drop(stmt);
    for p in paths {
        if !Path::new(&p).is_file() {
            tx.execute(
                "DELETE FROM items WHERE path=?1 AND prepared_at IS NULL",
                params![p],
            )
            .map_err(AppError::from)?;
        }
    }
    tx.commit().map_err(AppError::from)
}
pub fn saved(folder: &Path, path: &Path, name: &str, quality: &str) -> Result<(), AppError> {
    let db = connect()?;
    db.execute("INSERT INTO items(path,folder,item_key,item_name,captured_at,prepared_at,quality) VALUES(?1,?2,?3,?4,?5,NULL,?6)
 ON CONFLICT(path) DO UPDATE SET item_key=excluded.item_key,item_name=excluded.item_name,captured_at=excluded.captured_at,
 prepared_at=NULL,quality=excluded.quality",
 params![path.to_string_lossy().as_ref(),folder_key(folder),duplicates::normalize_item(name),name,Local::now().to_rfc3339(),quality])
 .map_err(AppError::from)?;
    Ok(())
}
pub fn stats(folder: &Path) -> Result<Stats, AppError> {
    let db = connect()?;
    let (total,prepared):(i64,i64)=db.query_row(
 "SELECT COUNT(*), COALESCE(SUM(CASE WHEN prepared_at IS NOT NULL THEN 1 ELSE 0 END),0) FROM items WHERE folder=?1",
 params![folder_key(folder)],|r|Ok((r.get(0)?,r.get(1)?))).map_err(AppError::from)?;
    Ok(Stats {
        total: total as usize,
        pending: (total - prepared) as usize,
        prepared: prepared as usize,
    })
}
pub fn prepared_paths(folder: &Path) -> Result<std::collections::HashSet<PathBuf>, AppError> {
    let db = connect()?;
    let mut stmt = db
        .prepare("SELECT path FROM items WHERE folder=?1 AND prepared_at IS NOT NULL")
        .map_err(AppError::from)?;
    let names = stmt
        .query_map(params![folder_key(folder)], |r| r.get::<_, String>(0))
        .map_err(AppError::from)?
        .collect::<Result<Vec<String>, _>>()
        .map_err(AppError::from)?
        .into_iter()
        .map(PathBuf::from)
        .collect();
    Ok(names)
}

pub fn item_prepared(folder: &Path, key: &str) -> Result<bool, AppError> {
    connect()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE folder=?1 AND item_key=?2 AND prepared_at IS NOT NULL)",
            params![folder_key(folder), key],
            |row| row.get(0),
        )
        .map_err(AppError::from)
}

pub fn recover_preparations(folder: &Path) -> Result<usize, AppError> {
    let _guard = duplicates::lock_capture_files()?;
    let directory = folder.join("Envois");
    duplicates::ensure_plain_child(folder, &directory)?;
    if !directory.is_dir() {
        return Ok(0);
    }
    let mut recovered = 0;
    for entry in fs::read_dir(&directory).map_err(AppError::from)? {
        let path = entry.map_err(AppError::from)?.path();
        if !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with(".preparation-"))
        {
            continue;
        }
        let journal: crate::batches::MoveJournal =
            serde_json::from_slice(&fs::read(&path).map_err(AppError::from)?)
                .map_err(|e| AppError::context("Journal de préparation illisible", e))?;
        let committed: bool = connect()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM export_operations WHERE id=?1)",
                params![journal.id],
                |r| r.get(0),
            )
            .map_err(AppError::from)?;
        if !committed {
            let has_parent =
                |p: &Path| p.components().any(|c| c == std::path::Component::ParentDir);
            for (source, target) in journal.files.iter().rev() {
                if source.parent() != Some(folder)
                    || !target.starts_with(&directory)
                    || has_parent(target)
                {
                    return Err(AppError::Domain(
                        crate::error::DomainError::JournalOutsideFolder,
                    ));
                }
                if target.exists() {
                    duplicates::ensure_plain_child(folder, target)?;
                    if !target
                        .canonicalize()
                        .map_err(AppError::from)?
                        .starts_with(directory.canonicalize().map_err(AppError::from)?)
                    {
                        return Err(AppError::Domain(
                            crate::error::DomainError::JournalEscapesExports,
                        ));
                    }
                    if source.exists() {
                        return Err(AppError::Domain(
                            crate::error::DomainError::RecoveryConflict,
                        ));
                    }
                    fs::rename(target, source).map_err(AppError::from)?;
                }
            }
            if let Some(batch) = journal
                .batch
                .filter(|p| p.parent() == Some(directory.as_path()))
            {
                let _ = fs::remove_file(batch.join("captures.csv"));
                for sub in journal
                    .directories
                    .iter()
                    .rev()
                    .filter(|p| p.parent() == Some(batch.as_path()))
                {
                    let _ = fs::remove_dir(sub);
                }
                let _ = fs::remove_dir(batch);
            }
            recovered += 1;
        }
        fs::remove_file(path).map_err(AppError::from)?;
    }
    Ok(recovered)
}

pub fn attach_evidence(folder: &Path, id: i64, path: &Path) -> Result<(), AppError> {
    connect()?
        .execute(
            "UPDATE visits SET evidence=?3 WHERE folder=?1 AND id=?2 AND evidence=''",
            params![folder_key(folder), id, path.to_string_lossy().as_ref()],
        )
        .map_err(AppError::from)?;
    Ok(())
}

pub fn rename_capture(folder: &Path, path: &Path, name: &str) -> Result<PathBuf, AppError> {
    let _guard = duplicates::lock_capture_files()?;
    if path.parent() != Some(folder) || !path.is_file() {
        return Err(AppError::Domain(
            crate::error::DomainError::CaptureOutsideFolder,
        ));
    }
    let name = name.trim();
    if name.is_empty() || crate::worker::safe_item_filename(name) != name {
        return Err(AppError::Domain(crate::error::DomainError::InvalidName));
    }
    let key = duplicates::normalize_item(name);
    if key.is_empty() {
        return Err(AppError::Domain(crate::error::DomainError::EmptyIdentity));
    }
    if duplicates::paths_for_item(folder, &key)?
        .iter()
        .any(|other| other != path)
        || item_prepared(folder, &key)?
    {
        return Err(AppError::Domain(
            crate::error::DomainError::DuplicateIdentity,
        ));
    }
    let target = folder.join(format!("{name}.png"));
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(AppError::Domain(
            crate::error::DomainError::ExistingFilename,
        ));
    }
    let mut db = connect()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(AppError::from)?;
    fs::rename(path, &target).map_err(AppError::from)?;
    let result = tx
        .execute(
            "UPDATE items SET path=?2,item_name=?3,item_key=?4 WHERE path=?1 AND folder=?5",
            params![
                path.to_string_lossy().as_ref(),
                target.to_string_lossy().as_ref(),
                name,
                key,
                folder_key(folder)
            ],
        )
        .and_then(|_| tx.commit());
    if let Err(error) = result {
        if let Err(restore) = fs::rename(&target, path) {
            return Err(AppError::Rollback {
                database: error,
                restore,
                retained: target.clone(),
            });
        }
        return Err(error.into());
    }
    duplicates::invalidate(folder);
    Ok(target)
}

pub struct ExportResult {
    pub path: PathBuf,
    pub moved: usize,
    pub archived: usize,
    pub skipped: usize,
}

#[derive(Default)]
struct PendingMoves {
    files: Vec<(PathBuf, PathBuf)>,
    committed: bool,
    batch: Option<PathBuf>,
    directories: Vec<PathBuf>,
    journal: Option<PathBuf>,
}
impl Drop for PendingMoves {
    fn drop(&mut self) {
        if !self.committed {
            let mut restored = true;
            for (source, destination) in self.files.iter().rev() {
                if source.exists() {
                    restored = false;
                    crate::diagnostics::write(
                        "ENVOI",
                        format!(
                            "Retour impossible, original présent : {}. Image conservée : {}",
                            source.display(),
                            destination.display()
                        ),
                    );
                } else if let Err(e) = fs::rename(destination, source) {
                    restored = false;
                    crate::diagnostics::write(
                        "ENVOI",
                        format!("Image conservée dans {} : {e}", destination.display()),
                    );
                }
            }
            if restored {
                if let Some(batch) = &self.batch {
                    let _ = fs::remove_file(batch.join("captures.csv"));
                    for directory in self.directories.iter().rev() {
                        let _ = fs::remove_dir(directory);
                    }
                    let _ = fs::remove_dir(batch);
                }
                if let Some(journal) = &self.journal {
                    let _ = fs::remove_file(journal);
                }
            }
        }
    }
}

pub fn prepare(folder: &Path, chunk_size: Option<usize>) -> Result<ExportResult, AppError> {
    prepare_selection(folder, chunk_size, None)
}
pub fn prepare_selection(
    folder: &Path,
    chunk_size: Option<usize>,
    selected: Option<&std::collections::HashSet<PathBuf>>,
) -> Result<ExportResult, AppError> {
    if chunk_size == Some(0) {
        return Err(AppError::Domain(
            crate::error::DomainError::InvalidChunkSize,
        ));
    }
    recover_preparations(folder)?;
    let _guard = duplicates::lock_capture_files()?;
    sync_files(folder)?;
    let batch_base = folder.join("Envois");
    let stamp = Local::now().format("%Y%m%d_%H%M%S").to_string();
    let mut batch = batch_base.join(format!("lot_{stamp}"));
    let mut index = 2;
    while batch.exists() {
        batch = batch_base.join(format!("lot_{stamp}_{index}"));
        index += 1;
    }
    let mut pending = Vec::new();
    let mut already_prepared = Vec::new();
    let mut skipped = 0;
    let prepared = prepared_paths(folder)?;
    for file in duplicates::png_files(folder)? {
        if selected.is_some_and(|paths| !paths.contains(&file)) {
            continue;
        }
        if duplicates::item_from_filename(&file).is_none() {
            skipped += 1;
            continue;
        }
        if prepared.contains(&file) {
            already_prepared.push(file);
            continue;
        }
        let (w, h) = match image::image_dimensions(&file) {
            Ok(size) => size,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if w < 230 || h < 210 {
            skipped += 1;
            continue;
        }
        pending.push(file);
    }
    pending.sort();
    let mut moves = PendingMoves::default();
    if !pending.is_empty() {
        fs::create_dir_all(&batch_base).map_err(AppError::from)?;
        fs::create_dir(&batch).map_err(AppError::from)?;
        moves.batch = Some(batch.clone());
        if let Some(limit) = chunk_size {
            for number in 1..=pending.len().div_ceil(limit) {
                let directory = batch.join(number.to_string());
                fs::create_dir(&directory).map_err(AppError::from)?;
                moves.directories.push(directory);
            }
        }
        let mut csv = String::from("objet;fichier;statut\n");
        for (position, file) in pending.iter().enumerate() {
            let name = file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace('"', "");
            let relative_path = match chunk_size {
                Some(limit) => format!("{}/{name}", position / limit + 1),
                None => name.clone(),
            };
            csv.push_str(&format!(
                "\"{}\";\"{}\";\"a_verifier_avant_envoi\"\n",
                name.trim_end_matches(".png"),
                relative_path
            ));
        }
        fs::write(batch.join("captures.csv"), csv).map_err(AppError::from)?;
    }
    let archive = batch_base.join("Deja_preparees");
    if !already_prepared.is_empty() {
        fs::create_dir_all(&archive).map_err(AppError::from)?;
    }
    let operation_id = format!(
        "{}-{}",
        std::process::id(),
        Local::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let planned = pending
        .iter()
        .enumerate()
        .map(|(position, file)| {
            let directory = match chunk_size {
                Some(limit) => batch.join((position / limit + 1).to_string()),
                None => batch.clone(),
            };
            (
                file.clone(),
                directory.join(file.file_name().unwrap_or_default()),
            )
        })
        .chain(already_prepared.iter().map(|file| {
            let name = file.file_name().unwrap_or_default();
            let mut target = archive.join(name);
            let mut suffix = 2;
            while target.exists() {
                target = archive.join(format!("{suffix}_{}", name.to_string_lossy()));
                suffix += 1;
            }
            (file.clone(), target)
        }))
        .collect::<Vec<_>>();
    if !planned.is_empty() {
        let journal = crate::batches::MoveJournal {
            id: operation_id.clone(),
            files: planned.clone(),
            batch: moves.batch.clone(),
            directories: moves.directories.clone(),
        };
        moves.journal = Some(crate::batches::write_journal(folder, &journal)?);
    }
    let mut db = connect()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(AppError::from)?;
    for (files, directory, split) in [
        (&pending, &batch, true),
        (&already_prepared, &archive, false),
    ] {
        for (position, file) in files.iter().enumerate() {
            let directory = match chunk_size.filter(|_| split) {
                Some(limit) => directory.join((position / limit + 1).to_string()),
                None => directory.clone(),
            };
            let name = file
                .file_name()
                .ok_or(AppError::Domain(crate::error::DomainError::MissingPngName))?;
            let mut target = directory.join(name);
            let mut suffix = 2;
            while target.exists() {
                target = directory.join(format!("{suffix}_{}", name.to_string_lossy()));
                suffix += 1;
            }
            let target = planned
                .iter()
                .find(|(source, _)| source == file)
                .map(|(_, target)| target.clone())
                .ok_or(AppError::Domain(crate::error::DomainError::UnjournaledMove))?;
            if target.exists() {
                return Err(AppError::Domain(crate::error::DomainError::ExportCollision));
            }
            fs::rename(file, &target)
                .map_err(|e| AppError::context(format!("Déplacement {}", file.display()), e))?;
            moves.files.push((file.clone(), target.clone()));
            tx.execute(
                "UPDATE items SET path=?2,prepared_at=COALESCE(prepared_at,?3) WHERE path=?1 AND folder=?4",
                params![file.to_string_lossy().as_ref(), target.to_string_lossy().as_ref(), Local::now().to_rfc3339(), folder_key(folder)],
            ).map_err(AppError::from)?;
        }
    }
    tx.execute(
        "INSERT INTO export_operations(id) VALUES(?1)",
        params![operation_id],
    )
    .map_err(AppError::from)?;
    tx.commit().map_err(AppError::from)?;
    moves.committed = true;
    duplicates::invalidate(folder);
    if let Some(journal) = &moves.journal {
        let _ = fs::remove_file(journal);
    }
    Ok(ExportResult {
        path: if pending.is_empty() { archive } else { batch },
        moved: pending.len(),
        archived: already_prepared.len(),
        skipped,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_export_and_rename_keep_other_images_unchanged() {
        let folder = export_folder("selection");
        let a = folder.join("Objet Alpha.png");
        let b = folder.join("Objet Beta.png");
        png(&a);
        png(&b);
        sync(&folder).unwrap();
        let bytes = fs::read(&b).unwrap();
        assert!(rename_capture(&folder, &a, "Objet Beta").is_err());
        let renamed = rename_capture(&folder, &a, "Objet Corrige").unwrap();
        assert!(!a.exists());
        assert!(renamed.exists());
        let selection = [renamed].into_iter().collect();
        let batch = prepare_selection(&folder, Some(1), Some(&selection)).unwrap();
        assert_eq!(batch.moved, 1);
        assert_eq!(fs::read(&b).unwrap(), bytes);
        assert!(batch.path.join("1/Objet Corrige.png").exists());
    }
    #[test]
    fn uncommitted_export_is_recovered_after_process_interruption() {
        let folder = export_folder("crash-rollback");
        let source = folder.join("Objet Conserve.png");
        png(&source);
        sync(&folder).unwrap();
        let batch = folder.join("Envois/lot_interrompu");
        fs::create_dir_all(&batch).unwrap();
        let target = batch.join("Objet Conserve.png");
        let journal = crate::batches::MoveJournal {
            id: "test-uncommitted".into(),
            files: vec![(source.clone(), target.clone())],
            batch: Some(batch.clone()),
            directories: vec![],
        };
        crate::batches::write_journal(&folder, &journal).unwrap();
        fs::rename(&source, &target).unwrap();
        assert_eq!(recover_preparations(&folder).unwrap(), 1);
        assert!(source.exists());
        assert!(!target.exists());
        assert_eq!(recover_preparations(&folder).unwrap(), 0);
    }
    #[test]
    fn committed_export_is_not_rolled_back_after_interruption() {
        let folder = export_folder("crash-commit");
        let source = folder.join("Objet Conserve.png");
        png(&source);
        let batch = prepare(&folder, None).unwrap();
        let target = batch.path.join("Objet Conserve.png");
        let id = format!(
            "test-committed-{}",
            Local::now().timestamp_nanos_opt().unwrap()
        );
        connect()
            .unwrap()
            .execute("INSERT INTO export_operations(id) VALUES(?1)", params![id])
            .unwrap();
        let journal = crate::batches::MoveJournal {
            id,
            files: vec![(source.clone(), target.clone())],
            batch: Some(batch.path),
            directories: vec![],
        };
        crate::batches::write_journal(&folder, &journal).unwrap();
        assert_eq!(recover_preparations(&folder).unwrap(), 0);
        assert!(target.exists());
        assert!(!source.exists());
    }
    #[test]
    fn concurrent_history_writes_are_not_lost() {
        let folder = export_folder("writers");
        let base = Local::now().timestamp_micros() * 1000;
        let workers = (0..12)
            .map(|worker| {
                let folder = folder.clone();
                std::thread::spawn(move || {
                    for sequence in 0..20 {
                        let record = VisitRecord {
                            id: base + worker * 1000 + sequence,
                            name: "Objet Test".into(),
                            state: VisitState::Saved,
                            reason: "test".into(),
                            observed_at: "test".into(),
                            evidence: String::new(),
                            session_id: "test".into(),
                        };
                        store_visit(&folder, &record).unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(recent_visits(&folder, 1000).unwrap().len(), 240);
    }
    #[test]
    fn stats_default_is_zero() {
        let s = Stats::default();
        assert_eq!(s.total, 0);
    }

    fn export_folder(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hdv-export-{label}-{}-{}",
            std::process::id(),
            Local::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    fn png(path: &Path) {
        image::RgbaImage::from_pixel(250, 230, image::Rgba([20, 40, 60, 255]))
            .save(path)
            .unwrap();
    }
    #[test]
    fn preparation_moves_images_once_and_remembers_them_after_batch_removal() {
        let folder = export_folder("move");
        let first = folder.join("Objet Alpha.png");
        let second = folder.join("Objet Bêta.png");
        png(&first);
        png(&second);
        let bytes = fs::read(&first).unwrap();
        let batch = prepare(&folder, None).unwrap();
        assert_eq!(batch.moved, 2);
        assert_eq!(batch.archived, 0);
        assert!(duplicates::png_files(&folder).unwrap().is_empty());
        assert_eq!(fs::read(batch.path.join("Objet Alpha.png")).unwrap(), bytes);
        assert!(batch.path.join("captures.csv").is_file());
        let again = prepare(&folder, None).unwrap();
        assert_eq!(again.moved, 0);
        assert!(!again.path.exists());
        assert_eq!(stats(&folder).unwrap().prepared, 2);
        // Even removing an already uploaded batch must not reset duplicate tracking.
        fs::remove_file(batch.path.join("Objet Alpha.png")).unwrap();
        sync(&folder).unwrap();
        assert!(item_prepared(&folder, "objet alpha").unwrap());
        let third = folder.join("Objet Gamma.png");
        png(&third);
        let next = prepare(&folder, None).unwrap();
        assert_eq!(next.moved, 1);
        assert_eq!(duplicates::png_files(&next.path).unwrap().len(), 1);
    }
    #[test]
    fn old_prepared_copies_are_archived_outside_the_new_batch() {
        let folder = export_folder("legacy");
        let old = folder.join("Objet Ancien.png");
        png(&old);
        saved(&folder, &old, "Objet Ancien", "test").unwrap();
        connect()
            .unwrap()
            .execute(
                "UPDATE items SET prepared_at='2026-01-01' WHERE path=?1",
                params![old.to_string_lossy().as_ref()],
            )
            .unwrap();
        let new = folder.join("Objet Nouveau.png");
        png(&new);
        let batch = prepare(&folder, None).unwrap();
        assert_eq!(batch.moved, 1);
        assert_eq!(batch.archived, 1);
        assert!(batch.path.join("Objet Nouveau.png").is_file());
        assert!(!batch.path.join("Objet Ancien.png").exists());
        assert!(folder
            .join("Envois/Deja_preparees/Objet Ancien.png")
            .is_file());
        assert!(duplicates::png_files(&folder).unwrap().is_empty());
        assert_eq!(prepare(&folder, None).unwrap().moved, 0);
    }
    #[test]
    fn split_batches_respect_limit_numbering_and_csv_paths() {
        for count in [450, 400] {
            let folder = export_folder(&format!("split-{count}"));
            let first = folder.join("Objet 0000.png");
            png(&first);
            let bytes = fs::read(first).unwrap();
            for i in 1..count {
                fs::write(folder.join(format!("Objet {i:04}.png")), &bytes).unwrap();
            }
            let batch = prepare(&folder, Some(200)).unwrap();
            assert_eq!(batch.moved, count);
            assert!(duplicates::png_files(&folder).unwrap().is_empty());
            assert!(duplicates::png_files(&batch.path).unwrap().is_empty());
            let groups = count.div_ceil(200);
            for group in 1..=groups {
                let expected = (count - (group - 1) * 200).min(200);
                assert_eq!(
                    duplicates::png_files(&batch.path.join(group.to_string()))
                        .unwrap()
                        .len(),
                    expected
                );
            }
            assert!(!batch.path.join((groups + 1).to_string()).exists());
            let csv = fs::read_to_string(batch.path.join("captures.csv")).unwrap();
            assert_eq!(csv.lines().count(), count + 1);
            for line in csv.lines().skip(1) {
                let relative = line.split(';').nth(1).unwrap().trim_matches('"');
                assert_eq!(fs::read(batch.path.join(relative)).unwrap(), bytes);
            }
            assert_eq!(prepare(&folder, Some(200)).unwrap().moved, 0);
        }
    }

    #[test]
    fn zero_split_limit_never_moves_a_capture() {
        let folder = export_folder("zero-limit");
        let path = folder.join("Objet Conserve.png");
        png(&path);
        let bytes = fs::read(&path).unwrap();
        assert!(prepare(&folder, Some(0)).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
        assert!(!folder.join("Envois").exists());
    }

    #[test]
    fn preparation_error_restores_original_file_and_leaves_it_pending() {
        let folder = export_folder("rollback");
        let file = folder.join("Objet Conserve.png");
        png(&file);
        let bytes = fs::read(&file).unwrap();
        sync(&folder).unwrap();
        let db = connect().unwrap();
        let escaped = folder_key(&folder).replace('\'', "''");
        db.execute_batch(&format!("CREATE TRIGGER test_export_failure BEFORE UPDATE ON items WHEN OLD.folder='{escaped}' BEGIN SELECT RAISE(ABORT, 'simulated write failure'); END;")).unwrap();
        drop(db);
        assert!(prepare(&folder, Some(1)).is_err());
        connect()
            .unwrap()
            .execute_batch("DROP TRIGGER test_export_failure")
            .unwrap();
        assert_eq!(fs::read(&file).unwrap(), bytes);
        assert_eq!(fs::read_dir(folder.join("Envois")).unwrap().count(), 0);
        assert_eq!(stats(&folder).unwrap().pending, 1);
        assert_eq!(prepare(&folder, None).unwrap().moved, 1);
    }
}

/// Historique des fiches réellement observées, y compris celles qui n'ont
/// jamais donné de PNG. Une entrée sans nom fiable reste « fiche non identifiée »
/// et ne doit jamais être présentée comme un objet précis.
#[path = "history_store.rs"]
mod history_store;
pub use history_store::{
    clear_visits_with_ids, delete_visit, export_missing, recent_visits, recover_interrupted,
    store_visit, visit_neighbors, VisitRecord,
};
