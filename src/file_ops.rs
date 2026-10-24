use crate::error::AppError;
use std::{
    fs,
    path::{Path, PathBuf},
};
pub fn archive(folder: &Path, paths: &[PathBuf]) -> Result<usize, AppError> {
    let _guard = crate::duplicates::lock_capture_files()?;
    let archive = folder.join("Corbeille_captures");
    crate::duplicates::ensure_plain_child(folder, &archive)?;
    fs::create_dir_all(&archive).map_err(AppError::from)?;
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for path in paths {
            if path.parent() != Some(folder) {
                return Err(AppError::Domain(
                    crate::error::DomainError::CaptureOutsideFolder,
                ));
            }
            let name = path.file_name().ok_or(AppError::Domain(
                crate::error::DomainError::MissingCaptureName,
            ))?;
            let mut target = archive.join(name);
            let mut suffix = 2;
            while target.exists() {
                target = archive.join(format!("{suffix}_{}", name.to_string_lossy()));
                suffix += 1;
            }
            fs::rename(path, &target).map_err(AppError::from)?;
            moved.push((path.clone(), target));
        }
        Ok(moved.len())
    })();
    if result.is_err() {
        for (source, target) in moved.iter().rev() {
            if !source.exists() {
                let _ = fs::rename(target, source);
            }
        }
    }
    crate::duplicates::invalidate(folder);
    result
}
