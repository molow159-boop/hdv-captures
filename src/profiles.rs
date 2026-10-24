use crate::error::AppError;
use crate::settings::Settings;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub settings: Settings,
}
pub fn load() -> Result<Vec<Profile>, AppError> {
    let path = crate::settings::config_path().with_file_name("profils.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(AppError::from)?).map_err(AppError::from)
}
pub fn store(profiles: &[Profile]) -> Result<(), AppError> {
    let path = crate::settings::config_path().with_file_name("profils.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(AppError::from)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(profiles).map_err(AppError::from)?,
    )
    .map_err(AppError::from)?;
    std::fs::rename(tmp, path).map_err(AppError::from)
}
