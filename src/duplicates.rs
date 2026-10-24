//! Un objet = une identité normalisée, indépendamment des pixels et de la date.
//! Recherche locale par nom pour éviter automatiquement les captures dupliquées.
use crate::error::AppError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

static CAPTURE_FILES: Mutex<()> = Mutex::new(());
pub fn ensure_plain_child(root: &Path, path: &Path) -> Result<(), AppError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AppError::Domain(crate::error::DomainError::PathOutsideFolder))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(AppError::Domain(
                crate::error::DomainError::InvalidPathComponent,
            ));
        };
        current.push(name);
        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            let mut link = metadata.file_type().is_symlink();
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                link |= metadata.file_attributes() & 0x400 != 0;
            }
            if link {
                return Err(AppError::Domain(
                    crate::error::DomainError::LinkedWorkingFolder,
                ));
            }
        }
    }
    Ok(())
}
struct Snapshot {
    modified: Option<SystemTime>,
    checked: Instant,
    items: HashMap<String, Vec<PathBuf>>,
}
static SNAPSHOTS: std::sync::LazyLock<Mutex<HashMap<PathBuf, Snapshot>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
pub fn invalidate(folder: &Path) {
    if let Ok(mut snapshots) = SNAPSHOTS.lock() {
        snapshots.remove(folder);
    }
}
pub fn saved(folder: &Path, path: &Path, name: &str) {
    if let Ok(mut snapshots) = SNAPSHOTS.lock() {
        if let Some(snapshot) = snapshots.get_mut(folder) {
            snapshot
                .items
                .insert(normalize_item(name), vec![path.to_path_buf()]);
            snapshot.modified = std::fs::metadata(folder).and_then(|m| m.modified()).ok();
        }
    }
}

pub fn lock_capture_files() -> Result<MutexGuard<'static, ()>, AppError> {
    CAPTURE_FILES
        .lock()
        .map_err(|_| AppError::Domain(crate::error::DomainError::CaptureFilesUnavailable))
}

/// Évite les faux doublons causés par accents, espaces, apostrophes et casse.
/// Ne fait PAS de rapprochement approximatif qui confondrait deux objets proches.
pub fn normalize_item(name: &str) -> String {
    let mut normalized = String::new();
    let mut last_space = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        let letters: &str = match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' => "a",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'î' | 'ï' | 'í' => "i",
            'ô' | 'ö' | 'ó' => "o",
            'ù' | 'û' | 'ü' | 'ú' => "u",
            'ç' => "c",
            'œ' => "oe",
            'æ' => "ae",
            _ => "",
        };
        if !letters.is_empty() {
            normalized.push_str(letters);
            last_space = false;
        } else if c.is_alphanumeric() {
            normalized.push(c);
            last_space = false;
        } else if !last_space && !normalized.is_empty() {
            normalized.push(' ');
            last_space = true;
        }
    }
    normalized.trim().to_owned()
}

fn looks_like_timestamp(s: &str) -> bool {
    // Ancien format : Nom objet__20261021_003950_082.png
    let chunks: Vec<_> = s.split('_').collect();
    chunks.len() >= 3
        && chunks[0].len() == 8
        && chunks[0].bytes().all(|b| b.is_ascii_digit())
        && chunks[1].len() == 6
        && chunks[1].bytes().all(|b| b.is_ascii_digit())
        && chunks[2].len() == 3
        && chunks[2].bytes().all(|b| b.is_ascii_digit())
        && (chunks.len() == 3 || (chunks.len() == 4 && chunks[3].parse::<u32>().is_ok()))
}

/// Identifie aussi les fichiers produits par les anciennes versions.
/// Les fichiers « HDV_<date>.png » ne contiennent pas le nom fiable.
pub fn item_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?.trim();
    if stem.is_empty()
        || stem.eq_ignore_ascii_case("Nom_non_reconnu")
        || stem.starts_with(".hdv-partial-")
        || stem.starts_with("Capture_sans_OCR__")
    {
        return None;
    }
    if stem.starts_with("HDV_") && stem[4..].chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let item = if let Some((name, suffix)) = stem.rsplit_once("__") {
        if looks_like_timestamp(suffix) {
            name
        } else {
            stem
        }
    } else {
        stem
    };
    let key = normalize_item(item);
    if key.is_empty() || key == "nom non reconnu" {
        None
    } else {
        Some(key)
    }
}

pub fn png_files(folder: &Path) -> Result<Vec<PathBuf>, AppError> {
    let iter = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(AppError::context("Lecture du dossier", e)),
    };
    Ok(iter
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && !p
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".hdv-partial-"))
                && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
        })
        .collect())
}

/// Cache disque réel, sans faire confiance aux anciennes images en mémoire.
/// Permet de ne pas recréer un objet déjà présent après redémarrage.
pub fn paths_for_item(folder: &Path, key: &str) -> Result<Vec<PathBuf>, AppError> {
    let modified = std::fs::metadata(folder).and_then(|m| m.modified()).ok();
    let mut snapshots = SNAPSHOTS
        .lock()
        .map_err(|_| AppError::Domain(crate::error::DomainError::DuplicateIndexUnavailable))?;
    if !snapshots.get(folder).is_some_and(|snapshot| {
        snapshot.modified == modified && snapshot.checked.elapsed() < Duration::from_secs(2)
    }) {
        let mut items: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in png_files(folder)? {
            if let Some(key) = item_from_filename(&path) {
                items.entry(key).or_default().push(path);
            }
        }
        if snapshots.len() >= 8 && !snapshots.contains_key(folder) {
            if let Some(old) = snapshots.keys().next().cloned() {
                snapshots.remove(&old);
            }
        }
        snapshots.insert(
            folder.to_path_buf(),
            Snapshot {
                modified,
                checked: Instant::now(),
                items,
            },
        );
    }
    Ok(snapshots
        .get(folder)
        .and_then(|snapshot| snapshot.items.get(key))
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|path| path.is_file())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_item_with_different_pixels_and_dates() {
        assert_eq!(
            normalize_item("Bottes Phossiles"),
            normalize_item("  bottes   PHOSSILES ")
        );
        assert_eq!(
            item_from_filename(Path::new("Bottes Phossiles__20261021_003950_082.png")),
            item_from_filename(Path::new("Bottes Phossiles.png"))
        );
        assert!(item_from_filename(Path::new("HDV_20261021_003950_082.png")).is_none());
    }
    #[test]
    fn accents_dont_create_duplicates_but_different_items_are_distinct() {
        assert_eq!(
            normalize_item("Épée du héros"),
            normalize_item("epee du heros")
        );
        assert_ne!(normalize_item("Cape Rice"), normalize_item("Cape Riche"));
    }
    #[test]
    fn file_lookup_normalizes_names_without_ocr() {
        let folder = std::env::temp_dir().join(format!("hdv-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Épée du héros.png"), b"test").unwrap();
        std::fs::write(folder.join("Cape Rice.png"), b"test").unwrap();
        let matches = paths_for_item(&folder, &normalize_item("epee du heros")).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].file_name().unwrap().to_string_lossy(),
            "Épée du héros.png"
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
