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
    #[serde(skip)]
    pub runtime_session: String,
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
            runtime_session: String::new(),
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

pub fn load_checked(path: &Path) -> (Settings, Option<String>) {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(settings) => (settings, None),
            Err(error) => {
                let backup = path.with_extension("json.bak");
                let recovered = std::fs::read(&backup)
                    .ok()
                    .and_then(|data| serde_json::from_slice(&data).ok());
                (recovered.unwrap_or_default(), Some(format!(
                    "Paramètres illisibles ({error}). Sauvegarde récupérée si disponible ; vérifie tes réglages.")))
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Settings::default(), None),
        Err(error) => (
            Settings::default(),
            Some(format!("Lecture des paramètres impossible : {error}")),
        ),
    }
}
pub fn load(path: &Path) -> Settings {
    load_checked(path).0
}

pub fn store(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(std::io::Error::other)?;
    static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let _guard = WRITER
        .lock()
        .map_err(|_| std::io::Error::other("Verrou des paramètres indisponible"))?;
    let tmp = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    if let Ok(previous) = std::fs::read(path) {
        if serde_json::from_slice::<Settings>(&previous).is_ok() {
            std::fs::write(path.with_extension("json.bak"), previous)?;
        }
    }
    let mut file = std::fs::File::create(&tmp)?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let result = std::fs::rename(&tmp, path);
    if result.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod gallery_tests {
    use super::*;
    #[test]
    fn invalid_configuration_recovers_last_valid_backup_without_overwriting_it() {
        let folder =
            std::env::temp_dir().join(format!("hdv-settings-recovery-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("config.json");
        let original = Settings {
            export_chunk_size: 123,
            ..Default::default()
        };
        store(&path, &original).unwrap();
        store(&path, &Settings::default()).unwrap();
        std::fs::write(&path, b"invalid").unwrap();
        let (recovered, warning) = load_checked(&path);
        assert!(warning.is_some());
        assert_eq!(recovered.export_chunk_size, 123);
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid");
        std::fs::remove_dir_all(folder).unwrap();
    }
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
