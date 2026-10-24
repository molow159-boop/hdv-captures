//! Local batch metadata and durable preparation journals. Never sends files.
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize, Debug)]
pub struct MoveJournal {
    pub id: String,
    pub files: Vec<(PathBuf, PathBuf)>,
    pub batch: Option<PathBuf>,
    pub directories: Vec<PathBuf>,
}

pub fn write_journal(folder: &Path, journal: &MoveJournal) -> Result<PathBuf, AppError> {
    let directory = folder.join("Envois");
    crate::duplicates::ensure_plain_child(folder, &directory)?;
    fs::create_dir_all(&directory).map_err(AppError::from)?;
    let path = directory.join(format!(".preparation-{}.json", journal.id));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(AppError::from)?;
    let data = serde_json::to_vec(journal).map_err(AppError::from)?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(AppError::from)?;
    Ok(path)
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub path: PathBuf,
    pub count: usize,
    pub sent: bool,
}

pub fn list(folder: &Path) -> Result<Vec<Batch>, AppError> {
    let directory = folder.join("Envois");
    crate::duplicates::ensure_plain_child(folder, &directory)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut batches = Vec::new();
    for entry in fs::read_dir(directory).map_err(AppError::from)? {
        let path = entry.map_err(AppError::from)?.path();
        if !path.is_dir()
            || !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("lot_"))
        {
            continue;
        }
        let mut count = crate::duplicates::png_files(&path)?.len();
        for child in fs::read_dir(&path).map_err(AppError::from)? {
            let child = child.map_err(AppError::from)?;
            if child.file_type().map_err(AppError::from)?.is_dir()
                && child.file_name().to_string_lossy().parse::<usize>().is_ok()
            {
                count += crate::duplicates::png_files(&child.path())?.len();
            }
        }
        let sent = path.join(".envoye.json").is_file();
        batches.push(Batch { path, count, sent });
    }
    batches.sort_by(|a, b| b.path.cmp(&a.path));
    Ok(batches)
}

pub fn mark_sent(folder: &Path, batch: &Path, sent: bool) -> Result<(), AppError> {
    crate::duplicates::ensure_plain_child(folder, batch)?;
    if batch.parent() != Some(folder.join("Envois").as_path())
        || !batch.is_dir()
        || !batch
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("lot_"))
    {
        return Err(AppError::Domain(
            crate::error::DomainError::BatchOutsideExports,
        ));
    }
    let marker = batch.join(".envoye.json");
    if sent {
        let tmp = marker.with_extension("json.tmp");
        let result = (|| {
            let mut file = fs::File::create(&tmp).map_err(AppError::from)?;
            file.write_all(
                format!(
                    "{{\"envoye_le\":\"{}\"}}",
                    chrono::Local::now().to_rfc3339()
                )
                .as_bytes(),
            )
            .map_err(AppError::from)?;
            file.sync_all().map_err(AppError::from)?;
            drop(file);
            fs::rename(&tmp, &marker).map_err(AppError::from)
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    } else if marker.exists() {
        fs::remove_file(marker).map_err(AppError::from)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_batch_state_persists_without_changing_images() {
        let folder = std::env::temp_dir().join(format!("hdv-lots-{}", std::process::id()));
        let batch = folder.join("Envois/lot_test");
        std::fs::create_dir_all(batch.join("1")).unwrap();
        let image = batch.join("1/image.png");
        std::fs::write(&image, b"image conservee").unwrap();
        let before = std::fs::read(&image).unwrap();
        assert_eq!(super::list(&folder).unwrap()[0].count, 1);
        super::mark_sent(&folder, &batch, true).unwrap();
        assert!(super::list(&folder).unwrap()[0].sent);
        super::mark_sent(&folder, &batch, false).unwrap();
        assert!(!super::list(&folder).unwrap()[0].sent);
        assert_eq!(std::fs::read(image).unwrap(), before);
        assert!(super::mark_sent(&folder, &folder, true).is_err());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
