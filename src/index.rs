//! Catalogue local SQLite (aucun envoi réseau).
use crate::duplicates;
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
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
        static TEST_DATABASE: OnceLock<PathBuf> = OnceLock::new();
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
static DATABASE_INIT: OnceLock<Result<(), String>> = OnceLock::new();
fn connect() -> Result<Connection, String> {
    let path = storage();
    DATABASE_INIT.get_or_init(|| {
  if let Some(dir)=path.parent(){fs::create_dir_all(dir).map_err(|e|e.to_string())?;}
  let init=Connection::open(&path).map_err(|e|e.to_string())?;
  init.busy_timeout(Duration::from_secs(1)).map_err(|e|e.to_string())?;
  init.execute_batch("PRAGMA journal_mode=WAL;
  CREATE TABLE IF NOT EXISTS items(path TEXT PRIMARY KEY,folder TEXT NOT NULL,item_key TEXT NOT NULL,item_name TEXT NOT NULL,
  captured_at TEXT NOT NULL,prepared_at TEXT,quality TEXT NOT NULL);
  CREATE INDEX IF NOT EXISTS idx_item ON items(folder,item_key);
  CREATE TABLE IF NOT EXISTS visits(
      id INTEGER PRIMARY KEY, folder TEXT NOT NULL, item_name TEXT NOT NULL DEFAULT '',
      state TEXT NOT NULL, reason TEXT NOT NULL, observed_at TEXT NOT NULL,
      evidence TEXT NOT NULL DEFAULT '', session_id TEXT NOT NULL DEFAULT '');
  CREATE INDEX IF NOT EXISTS idx_visits ON visits(folder,id DESC);")
      .map_err(|e|e.to_string())?;
  // Migration idempotente pour les utilisateurs d'une version de test 0.2.5.
  let _=init.execute("ALTER TABLE visits ADD COLUMN evidence TEXT NOT NULL DEFAULT ''",[]);
  let _=init.execute("ALTER TABLE visits ADD COLUMN session_id TEXT NOT NULL DEFAULT ''",[]);
  Ok(())
 }).as_ref().map_err(|e|e.clone())?;
    // Ne jamais bloquer l'interface pendant quatre secondes sur un verrou SQL.
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_millis(300))
        .map_err(|e| e.to_string())?;
    Ok(conn)
}
fn folder_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
pub fn sync(folder: &Path) -> Result<(), String> {
    let _guard = duplicates::lock_capture_files()?;
    sync_files(folder)
}

fn sync_files(folder: &Path) -> Result<(), String> {
    let mut db = connect()?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    for file in duplicates::png_files(folder)? {
        let Some(key) = duplicates::item_from_filename(&file) else {
            continue;
        };
        let label = file.file_stem().unwrap_or_default().to_string_lossy();
        let label = label.split("__").next().unwrap_or(&label).to_owned();
        tx.execute("INSERT OR IGNORE INTO items(path,folder,item_key,item_name,captured_at,quality) VALUES (?1,?2,?3,?4,?5,'historique à vérifier')",
    params![file.to_string_lossy().as_ref(),folder_key(folder),key,label,Local::now().to_rfc3339()])
    .map_err(|e|e.to_string())?;
    }
    let mut stmt = tx
        .prepare("SELECT path FROM items WHERE folder=?1")
        .map_err(|e| e.to_string())?;
    let paths = stmt
        .query_map(params![folder_key(folder)], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();
    drop(stmt);
    for p in paths {
        if !Path::new(&p).is_file() {
            tx.execute(
                "DELETE FROM items WHERE path=?1 AND prepared_at IS NULL",
                params![p],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}
pub fn saved(folder: &Path, path: &Path, name: &str, quality: &str) -> Result<(), String> {
    let db = connect()?;
    db.execute("INSERT INTO items(path,folder,item_key,item_name,captured_at,prepared_at,quality) VALUES(?1,?2,?3,?4,?5,NULL,?6)
 ON CONFLICT(path) DO UPDATE SET item_key=excluded.item_key,item_name=excluded.item_name,captured_at=excluded.captured_at,
 prepared_at=NULL,quality=excluded.quality",
 params![path.to_string_lossy().as_ref(),folder_key(folder),duplicates::normalize_item(name),name,Local::now().to_rfc3339(),quality])
 .map_err(|e|e.to_string())?;
    Ok(())
}
pub fn stats(folder: &Path) -> Result<Stats, String> {
    let db = connect()?;
    let (total,prepared):(i64,i64)=db.query_row(
 "SELECT COUNT(*), COALESCE(SUM(CASE WHEN prepared_at IS NOT NULL THEN 1 ELSE 0 END),0) FROM items WHERE folder=?1",
 params![folder_key(folder)],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|e|e.to_string())?;
    Ok(Stats {
        total: total as usize,
        pending: (total - prepared) as usize,
        prepared: prepared as usize,
    })
}
pub fn prepared_paths(folder: &Path) -> Result<std::collections::HashSet<PathBuf>, String> {
    let db = connect()?;
    let mut stmt = db
        .prepare("SELECT path FROM items WHERE folder=?1 AND prepared_at IS NOT NULL")
        .map_err(|e| e.to_string())?;
    let names = stmt
        .query_map(params![folder_key(folder)], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(PathBuf::from)
        .collect();
    Ok(names)
}
fn was_prepared(path: &Path) -> Result<bool, String> {
    let db = connect()?;
    let found: Option<String> = db
        .query_row(
            "SELECT prepared_at FROM items WHERE path=?1",
            params![path.to_string_lossy().as_ref()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();
    Ok(found.is_some())
}

pub fn item_prepared(folder: &Path, key: &str) -> Result<bool, String> {
    connect()?
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE folder=?1 AND item_key=?2 AND prepared_at IS NOT NULL)",
            params![folder_key(folder), key],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
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
                        &format!(
                            "Retour impossible, original présent : {}. Image conservée : {}",
                            source.display(),
                            destination.display()
                        ),
                    );
                } else if let Err(e) = fs::rename(destination, source) {
                    restored = false;
                    crate::diagnostics::write(
                        "ENVOI",
                        &format!("Image conservée dans {} : {e}", destination.display()),
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
            }
        }
    }
}

pub fn prepare(folder: &Path, chunk_size: Option<usize>) -> Result<ExportResult, String> {
    if chunk_size == Some(0) {
        return Err("Le maximum de captures par dossier doit être au moins égal à 1.".into());
    }
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
    for file in duplicates::png_files(folder)? {
        if duplicates::item_from_filename(&file).is_none() {
            skipped += 1;
            continue;
        }
        if was_prepared(&file)? {
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
        fs::create_dir_all(&batch_base).map_err(|e| e.to_string())?;
        fs::create_dir(&batch).map_err(|e| e.to_string())?;
        moves.batch = Some(batch.clone());
        if let Some(limit) = chunk_size {
            for number in 1..=pending.len().div_ceil(limit) {
                let directory = batch.join(number.to_string());
                fs::create_dir(&directory).map_err(|e| e.to_string())?;
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
        fs::write(batch.join("captures.csv"), csv).map_err(|e| e.to_string())?;
    }
    let archive = batch_base.join("Deja_preparees");
    if !already_prepared.is_empty() {
        fs::create_dir_all(&archive).map_err(|e| e.to_string())?;
    }
    let mut db = connect()?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    for (files, directory, split) in [
        (&pending, &batch, true),
        (&already_prepared, &archive, false),
    ] {
        for (position, file) in files.iter().enumerate() {
            let directory = match chunk_size.filter(|_| split) {
                Some(limit) => directory.join((position / limit + 1).to_string()),
                None => directory.clone(),
            };
            let name = file.file_name().ok_or("PNG sans nom")?;
            let mut target = directory.join(name);
            let mut suffix = 2;
            while target.exists() {
                target = directory.join(format!("{suffix}_{}", name.to_string_lossy()));
                suffix += 1;
            }
            fs::rename(file, &target)
                .map_err(|e| format!("Déplacement {} : {e}", file.display()))?;
            moves.files.push((file.clone(), target.clone()));
            tx.execute(
                "UPDATE items SET path=?2,prepared_at=COALESCE(prepared_at,?3) WHERE path=?1 AND folder=?4",
                params![file.to_string_lossy().as_ref(), target.to_string_lossy().as_ref(), Local::now().to_rfc3339(), folder_key(folder)],
            ).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    moves.committed = true;
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
        assert!(prepare(&folder, Some(1)).is_err());
        db.execute_batch("DROP TRIGGER test_export_failure")
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
#[derive(Clone, Debug)]
pub struct VisitRecord {
    pub id: i64,
    pub name: String,
    pub state: String,
    pub reason: String,
    pub observed_at: String,
    pub evidence: String,
    /// Identifiant de lancement: ne pas lier des objets de deux sessions.
    pub session_id: String,
}
pub fn store_visit(folder: &Path, v: &VisitRecord) -> Result<(), String> {
    let db = connect()?;
    db.execute("INSERT INTO visits(id,folder,item_name,state,reason,observed_at,evidence,session_id)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(id) DO UPDATE SET
          item_name=CASE WHEN excluded.item_name <> '' THEN excluded.item_name ELSE visits.item_name END,
          state=excluded.state, reason=excluded.reason,
          session_id=CASE WHEN visits.session_id = '' THEN excluded.session_id ELSE visits.session_id END,
          evidence=CASE WHEN excluded.evidence <> '' THEN excluded.evidence ELSE visits.evidence END",
        params![v.id,folder_key(folder),v.name,v.state,v.reason,v.observed_at,v.evidence,v.session_id])
        .map_err(|e|e.to_string())?;
    Ok(())
}
/// Ne modifie jamais les PNG ni les images de contrôle ; uniquement le suivi SQLite.
pub fn delete_visit(folder: &Path, id: i64) -> Result<usize, String> {
    let db = connect()?;
    db.execute(
        "DELETE FROM visits WHERE folder=?1 AND id=?2",
        params![folder_key(folder), id],
    )
    .map_err(|e| e.to_string())
}

/// Select and remove the same rows atomically, returning tombstones for late OCR.
pub fn clear_visits_with_ids(folder: &Path, only_missing: bool) -> Result<Vec<i64>, String> {
    let mut db = connect()?;
    clear_visits_on(&mut db, &folder_key(folder), only_missing)
}

fn clear_visits_on(
    db: &mut Connection,
    folder: &str,
    only_missing: bool,
) -> Result<Vec<i64>, String> {
    // Reserve the writer before reading ids: a deferred read transaction cannot
    // be upgraded if a concurrent gallery/export write changes its WAL snapshot.
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let condition = if only_missing {
        "folder=?1 AND state IN ('a_reprendre','en_attente')"
    } else {
        "folder=?1"
    };
    let ids = {
        let mut statement = tx
            .prepare(&format!("SELECT id FROM visits WHERE {condition}"))
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![folder], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    tx.execute(
        &format!("DELETE FROM visits WHERE {condition}"),
        params![folder],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ids)
}

pub fn recent_visits(folder: &Path, limit: usize) -> Result<Vec<VisitRecord>, String> {
    let db = connect()?;
    let mut stmt = db
        .prepare(
            "SELECT id,item_name,state,reason,observed_at,evidence,session_id FROM visits
                             WHERE folder=?1 ORDER BY id DESC LIMIT ?2",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![folder_key(folder), limit as i64], |row| {
            Ok(VisitRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                state: row.get(2)?,
                reason: row.get(3)?,
                observed_at: row.get(4)?,
                evidence: row.get(5)?,
                session_id: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(Result::ok).collect())
}
/// Repères de navigation pour une fiche anonyme, sans inventer son identité.
/// `visits` est trié du plus récent au plus ancien (ordre de détection, pas
/// de fin d'OCR). Les noms sont fiables UNIQUEMENT après sauvegarde.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Neighbors {
    pub before: Option<String>,
    pub after: Option<String>,
    pub ambiguous: usize,
}
pub fn visit_neighbors(visits: &[VisitRecord], index: usize) -> Neighbors {
    let Some(v) = visits.get(index) else {
        return Neighbors::default();
    };
    if v.session_id.is_empty() {
        return Neighbors::default();
    }
    let named = |r: &VisitRecord| {
        r.session_id == v.session_id && r.state == "enregistre" && !r.name.trim().is_empty()
    };
    // Les IDs sont attribués à l'ouverture, avant que l'OCR asynchrone finisse.
    let before_idx = (index + 1..visits.len()).find(|&i| named(&visits[i]));
    let after_idx = (0..index).rev().find(|&i| named(&visits[i]));
    let before = before_idx.map(|i| visits[i].name.clone());
    let after = after_idx.map(|i| visits[i].name.clone());
    let ambiguous = match (before_idx, after_idx) {
        (Some(b), Some(a)) if a + 1 < b => visits[a + 1..b]
            .iter()
            .filter(|r| r.session_id == v.session_id && r.state != "enregistre")
            .count(),
        _ => 0,
    };
    Neighbors {
        before,
        after,
        ambiguous,
    }
}

pub fn export_missing(folder: &Path) -> Result<(PathBuf, usize), String> {
    let visits = recent_visits(folder, 10000)?;
    let path = folder.join("Objets_a_reprendre.csv");
    let mut text=String::from("\u{FEFF}date;objet;statut;raison;capture_de_controle;objet_avant;objet_apres;fiches_non_confirmees_entre_les_deux\n");
    let mut count = 0;
    for (i, v) in visits.iter().enumerate() {
        if v.state != "a_reprendre" && v.state != "en_attente" {
            continue;
        }
        count += 1;
        let neighbors = visit_neighbors(&visits, i);
        let clean = |s: &str| s.replace('\"', "\"\"").replace(['\n', '\r'], " ");
        text.push_str(&format!(
            "\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";{}\n",
            clean(&v.observed_at),
            clean(&v.name),
            clean(&v.state),
            clean(&v.reason),
            clean(&v.evidence),
            clean(neighbors.before.as_deref().unwrap_or("")),
            clean(neighbors.after.as_deref().unwrap_or("")),
            neighbors.ambiguous
        ));
    }
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok((path, count))
}

/// Après un arrêt forcé, aucune lecture d'une session précédente ne doit
/// rester indéfiniment affichée comme « en cours ».
pub fn recover_interrupted(folder: &Path) -> Result<usize, String> {
    let db = connect()?;
    let n = db
        .execute(
            "UPDATE visits SET state='a_reprendre',
        reason='Capture interrompue avant confirmation' WHERE folder=?1 AND state='en_attente'",
            params![folder_key(folder)],
        )
        .map_err(|e| e.to_string())?;
    Ok(n)
}

#[cfg(test)]
mod neighbor_tests {
    use super::*;
    fn v(id: i64, name: &str, state: &str, session: &str) -> VisitRecord {
        VisitRecord {
            id,
            name: name.into(),
            state: state.into(),
            reason: String::new(),
            observed_at: String::new(),
            evidence: String::new(),
            session_id: session.into(),
        }
    }
    #[test]
    fn anonymous_gap_is_framed_by_known_items_not_guessed() {
        let visits = vec![
            v(5, "Après", "enregistre", "A"),
            v(4, "", "a_reprendre", "A"),
            v(3, "", "a_reprendre", "A"),
            v(2, "Avant", "enregistre", "A"),
        ];
        let c = visit_neighbors(&visits, 1);
        assert_eq!(c.before.as_deref(), Some("Avant"));
        assert_eq!(c.after.as_deref(), Some("Après"));
        assert_eq!(c.ambiguous, 2);
    }
    #[test]
    fn sessions_are_not_connected_even_when_ordered() {
        let visits = vec![
            v(5, "Après", "enregistre", "B"),
            v(4, "", "a_reprendre", "B"),
            v(3, "Avant", "enregistre", "A"),
        ];
        let c = visit_neighbors(&visits, 1);
        assert!(c.before.is_none());
        assert_eq!(c.after.as_deref(), Some("Après"));
    }
    #[test]
    fn incomplete_names_and_legacy_without_session_are_not_used() {
        let visits = vec![
            v(3, "Après", "enregistre", ""),
            v(2, "", "a_reprendre", ""),
            v(1, "Avant", "enregistre", ""),
        ];
        assert_eq!(visit_neighbors(&visits, 1), Neighbors::default());
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    #[test]
    fn deletion_scopes_never_touch_items_or_other_folders() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE visits(id INTEGER PRIMARY KEY,folder TEXT,state TEXT);
            CREATE TABLE items(path TEXT PRIMARY KEY);
            INSERT INTO visits VALUES(1,'A','enregistre'),(2,'A','a_reprendre'),
                (3,'A','en_attente'),(4,'B','a_reprendre');
            INSERT INTO items VALUES('capture.png');",
        )
        .unwrap();
        let mut removed = clear_visits_on(&mut db, "A", true).unwrap();
        removed.sort_unstable();
        assert_eq!(removed, vec![2, 3]);
        assert_eq!(
            db.query_row("SELECT count(*) FROM visits", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // A new observation after clearing must remain independently removable.
        db.execute("INSERT INTO visits VALUES(5,'A','en_attente')", [])
            .unwrap();
        let mut removed = clear_visits_on(&mut db, "A", false).unwrap();
        removed.sort_unstable();
        assert_eq!(removed, vec![1, 5]);
        assert_eq!(
            db.query_row("SELECT id FROM visits", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
}
