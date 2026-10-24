//! Storage inspection never follows directory links; cleanup requires an explicit preview.
use crate::error::AppError;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
#[derive(Default, Clone)]
pub struct Usage {
    pub bytes: u64,
    pub files: usize,
    pub cleanup: Vec<Candidate>,
}
#[derive(Clone)]
pub struct Candidate {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}
impl Candidate {
    pub fn preview(&self, folder: &Path) -> String {
        format!(
            "{} · {:.1} Kio",
            self.path
                .strip_prefix(folder)
                .unwrap_or(&self.path)
                .display(),
            self.bytes as f64 / 1024.0
        )
    }
}
pub fn inspect(folder: &Path, days: u64) -> Result<Usage, AppError> {
    let mut usage = Usage::default();
    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(days.saturating_mul(86400)))
        .ok_or(AppError::Domain(crate::error::DomainError::InvalidDuration))?;
    for name in [
        "",
        "Fiches_a_reprendre",
        "Captures_remplacees",
        "Corbeille_captures",
        "Envois",
    ] {
        let root = folder.join(name);
        crate::duplicates::ensure_plain_child(folder, &root)?;
        if !root.is_dir() {
            continue;
        }
        visit(
            &root,
            !name.is_empty(),
            matches!(
                name,
                "Fiches_a_reprendre" | "Captures_remplacees" | "Corbeille_captures"
            ),
            cutoff,
            &mut usage,
        )?;
    }
    Ok(usage)
}
fn visit(
    folder: &Path,
    recurse: bool,
    eligible: bool,
    cutoff: SystemTime,
    usage: &mut Usage,
) -> Result<(), AppError> {
    for entry in fs::read_dir(folder).map_err(AppError::from)? {
        let entry = entry.map_err(AppError::from)?;
        let kind = entry.file_type().map_err(AppError::from)?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() && recurse {
            visit(&entry.path(), true, eligible, cutoff, usage)?;
        } else if kind.is_file() {
            let metadata = entry.metadata().map_err(AppError::from)?;
            usage.files += 1;
            usage.bytes = usage.bytes.saturating_add(metadata.len());
            if eligible
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            {
                let modified = metadata.modified().map_err(AppError::from)?;
                if modified < cutoff {
                    usage.cleanup.push(Candidate {
                        path: entry.path(),
                        bytes: metadata.len(),
                        modified,
                    });
                }
            }
        }
    }
    Ok(())
}
pub fn cleanup(folder: &Path, plan: &[Candidate]) -> Result<usize, AppError> {
    let _guard = crate::duplicates::lock_capture_files()?;
    let roots = [
        "Fiches_a_reprendre",
        "Captures_remplacees",
        "Corbeille_captures",
    ]
    .map(|n| folder.join(n));
    let mut count = 0;
    for candidate in plan {
        crate::duplicates::ensure_plain_child(folder, &candidate.path)?;
        if !roots.iter().any(|r| candidate.path.starts_with(r))
            || candidate
                .path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(AppError::Domain(
                crate::error::DomainError::CleanupOutsideArchives,
            ));
        }
        if !candidate.path.exists() {
            continue;
        }
        let canonical = candidate.path.canonicalize().map_err(AppError::from)?;
        if !roots
            .iter()
            .filter_map(|r| r.canonicalize().ok())
            .any(|r| canonical.starts_with(r))
        {
            return Err(AppError::Domain(crate::error::DomainError::LinkedArchive));
        }
        let metadata = fs::symlink_metadata(&candidate.path).map_err(AppError::from)?;
        if metadata.is_file()
            && metadata.len() == candidate.bytes
            && metadata.modified().ok() == Some(candidate.modified)
        {
            fs::remove_file(&candidate.path).map_err(AppError::from)?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cleanup_preview_is_age_limited_and_never_deletes_a_changed_file() {
        let folder = std::env::temp_dir().join(format!("hdv-cleanup-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("Corbeille_captures")).unwrap();
        let old = folder.join("Corbeille_captures/ancien.png");
        std::fs::write(&old, b"archive").unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&old).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86400),
        ))
        .unwrap();
        drop(file);
        let plan = super::inspect(&folder, 30).unwrap();
        assert_eq!(plan.cleanup.len(), 1);
        std::fs::write(&old, b"archive modifiee").unwrap();
        assert_eq!(super::cleanup(&folder, &plan.cleanup).unwrap(), 0);
        assert!(old.exists());
        let file = std::fs::OpenOptions::new().write(true).open(&old).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(
            std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86400),
        ))
        .unwrap();
        drop(file);
        let plan = super::inspect(&folder, 30).unwrap();
        assert_eq!(super::cleanup(&folder, &plan.cleanup).unwrap(), 1);
        assert!(!old.exists());
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn active_images_and_batches_are_never_cleanup_candidates() {
        let folder = std::env::temp_dir().join(format!("hdv-storage-{}", std::process::id()));
        std::fs::create_dir_all(folder.join("Envois/lot_test")).unwrap();
        std::fs::write(folder.join("image.png"), b"active").unwrap();
        std::fs::write(folder.join("Envois/lot_test/image.png"), b"batch").unwrap();
        let usage = super::inspect(&folder, 0).unwrap();
        assert_eq!(usage.files, 2);
        assert!(usage.cleanup.is_empty());
        std::fs::remove_dir_all(folder).unwrap();
    }
}
