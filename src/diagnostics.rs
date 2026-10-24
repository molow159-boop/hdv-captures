//! Diagnostic local et borné en taille, aucune télémétrie.
use chrono::Local;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};
pub fn path() -> PathBuf {
    crate::settings::config_path().with_file_name("diagnostic.log")
}
pub fn write(kind: &str, detail: impl std::fmt::Display) {
    let detail = detail.to_string();
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::metadata(&path).is_ok_and(|m| m.len() > 2_000_000) {
        let old = path.with_extension("log.1");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&path, old);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let one_line = detail.replace(['\r', '\n'], " ");
        let _ = writeln!(
            file,
            "{} [{}] {}",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            kind,
            one_line.chars().take(360).collect::<String>()
        );
    }
}
pub fn last_lines(n: usize) -> String {
    match fs::read_to_string(path()) {
        Ok(c) => c
            .lines()
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n"),
        Err(_) => "Pas de diagnostic enregistré.".into(),
    }
}
