use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CaptureRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub automatic: bool,
    pub compact: bool,
    /// La collecte fonctionne aussi quand les panneaux galerie et aperçu sont masques.
    pub show_gallery: bool,
    pub screen_index: usize,
    pub region: Option<CaptureRect>,
    pub destination: PathBuf,
    pub hotkey: String,
    pub ocr_exe: Option<PathBuf>,
    pub alert_sound: bool,
    pub split_exports: bool,
    pub export_chunk_size: usize,
}

impl Default for Settings {
    fn default() -> Self {
        let base = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            automatic: false,
            compact: false,
            show_gallery: true,
            screen_index: 0,
            region: None,
            destination: base.join("Pictures").join("HDV Captures"),
            hotkey: "Ctrl + Maj + S".into(),
            ocr_exe: None,
            alert_sound: false,
            split_exports: false,
            export_chunk_size: 200,
        }
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("HDV Captures").join("config.json")
}

pub fn load(path: &Path) -> Settings {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .unwrap_or_default()
}

pub fn store(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod gallery_tests {
    use super::*;
    #[test]
    fn old_settings_default_to_visible_gallery() {
        let saved = r#"{"automatic":false,"compact":false}"#;
        let settings: Settings = serde_json::from_str(saved).expect("ancien fichier valide");
        assert!(settings.show_gallery);
        assert!(!settings.split_exports);
        assert_eq!(settings.export_chunk_size, 200);
    }
    #[test]
    fn gallery_setting_persists() {
        let settings = Settings {
            show_gallery: false,
            ..Settings::default()
        };
        let decoded: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert!(!decoded.show_gallery);
    }

    #[test]
    fn export_split_option_and_limit_survive_restart() {
        let path =
            std::env::temp_dir().join(format!("hdv-split-settings-{}.json", std::process::id()));
        let settings = Settings {
            split_exports: true,
            export_chunk_size: 173,
            ..Settings::default()
        };
        store(&path, &settings).unwrap();
        let loaded = load(&path);
        assert!(loaded.split_exports);
        assert_eq!(loaded.export_chunk_size, 173);
        std::fs::remove_file(path).unwrap();
    }
}
