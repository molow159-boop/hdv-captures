//! Reconnaissance VISUELLE des noms. Aucun accès au processus Dofus.
//! En mode automatique, une fiche sans OCR lisible n'est pas envoyée.
use crate::settings::CaptureRect;
use image::{imageops, RgbaImage};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static OCR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name {
    Complete(String),
    Truncated(String),
    Unreadable,
}

#[derive(Debug, Clone)]
pub struct Hover {
    pub name: String,
    pub text_rect: CaptureRect,
}

fn candidates(
    configured: Option<&Path>,
    app_dir: Option<&Path>,
    program_files: Option<&Path>,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = configured {
        paths.push(path.to_path_buf());
    }
    if let Some(dir) = app_dir {
        paths.push(dir.join("ocr").join("tesseract.exe"));
    }
    paths.push(PathBuf::from("tesseract"));
    if let Some(program_files) = program_files {
        paths.push(
            PathBuf::from(program_files)
                .join("Tesseract-OCR")
                .join("tesseract.exe"),
        );
    }
    paths
}

pub fn bundled_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join("ocr").join("tesseract.exe"))
}

fn command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    // Use the data shipped with this executable, independently of a system
    // installation or a stale TESSDATA_PREFIX inherited from another program.
    if let Some(parent) = exe.parent().filter(|p| !p.as_os_str().is_empty()) {
        let data = parent.join("tessdata");
        if data.is_dir() {
            cmd.env_remove("TESSDATA_PREFIX");
            cmd.arg("--tessdata-dir").arg(data);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

pub fn locate(configured: Option<&Path>) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok();
    let app_dir = executable.as_deref().and_then(Path::parent);
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    for path in candidates(configured, app_dir, program_files.as_deref()) {
        if let Ok(result) = command(&path).arg("--version").output() {
            if result.status.success() {
                return Some(path);
            }
        }
    }
    None
}

pub fn check_languages(exe: &Path) -> Result<(), String> {
    let mut cmd = command(exe);
    cmd.arg("--list-langs");
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err("Tesseract indisponible".into());
    }
    let all = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for lang in ["fra", "eng"] {
        if !all.lines().any(|s| s.trim() == lang) {
            return Err(format!("Langue {lang} manquante"));
        }
    }
    Ok(())
}
fn recognize(image: &RgbaImage, exe: &Path, psm: u8, output: &str) -> Result<String, String> {
    if image.width() < 10 || image.height() < 10 {
        return Err("Zone OCR trop petite".into());
    }
    let tmp = std::env::temp_dir().join(format!(
        "hdv-ocr-{}-{}.png",
        std::process::id(),
        OCR_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    image
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(|e| format!("Préparation OCR : {e}"))?;
    let mut cmd = command(exe);
    cmd.arg(&tmp)
        .arg("stdout")
        .arg("-l")
        .arg("fra+eng")
        .arg("--psm")
        .arg(psm.to_string())
        .arg(output);
    let result = cmd.output();
    let _ = std::fs::remove_file(&tmp);
    let result = result.map_err(|e| format!("Tesseract inaccessible : {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "Tesseract : vérifie le moteur et les langues fra et eng ({})",
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(130)
                .collect::<String>()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).into_owned())
}

/// Les versions et thèmes de Dofus modifient légèrement l'apparence des
/// libellés. On exige deux indications convergentes, pas trois mots exacts.
fn metadata_score(raw: &str) -> u8 {
    let compact = normalize(raw);
    let price = compact.contains("prixmoy")
        || compact.contains("prismoy")
        || compact.contains("prixmoven")
        || compact.contains("prixmoyen");
    let stock = compact.contains("quantite")
        || compact.contains("inventaire")
        || compact.contains("banque")
        || compact.contains("havresac");
    let level = compact.contains("niv") || compact.contains("niveau");
    price as u8 + stock as u8 + level as u8
}

/// Lecture par défaut en un passage, secours seulement lorsque nécessaire.
/// Cela évite la
/// double attente qui faisait rater des objets parcourus rapidement.
/// None signifie que la liste ou une fiche non prête a été détectée.
pub fn analyze_panel(
    frame: &RgbaImage,
    panel: CaptureRect,
    exe: &Path,
) -> Result<Option<Name>, String> {
    let area = CaptureRect {
        x: panel.x.saturating_add(77),
        y: panel.y.saturating_add(40),
        width: panel.width.saturating_sub(86),
        height: 148,
    };
    let source = crate::vision::crop(frame, area).ok_or("En-tête HDV hors écran")?;
    let resized = imageops::resize(
        &source,
        source.width() * 2,
        source.height() * 2,
        imageops::FilterType::CatmullRom,
    );
    let first = recognize(&resized, exe, 6, "txt")?;
    // La structure des prix est DÉJÀ validée par le détecteur visuel.
    // Ne pas refuser systématiquement une vraie fiche parce que le texte
    // « Prix moyen » ou « Quantité » est mal lu par Tesseract.
    let score = metadata_score(&first);
    let candidate = extract_name(&first);
    let verdict = if score >= 1 && !matches!(candidate, Name::Unreadable) {
        candidate
    } else {
        // Secours : seule la ligne de nom, agrandie et lue indépendamment.
        // Utile lorsque la disposition ou l’échelle de l'UI varie.
        let scale = panel.width as f32 / 413.0;
        let offset_x = (79.0 * scale).round() as u32;
        let offset_y = (41.0 * scale).round() as u32;
        let title_area = CaptureRect {
            x: panel.x.saturating_add(offset_x),
            y: panel.y.saturating_add(offset_y),
            width: panel
                .width
                .saturating_sub(offset_x)
                .saturating_sub((15.0 * scale).round() as u32),
            height: (30.0 * scale).round() as u32,
        };
        let source = crate::vision::crop(frame, title_area).ok_or("Titre de fiche hors écran")?;
        let enlarged = imageops::resize(
            &source,
            source.width() * 3,
            source.height() * 3,
            imageops::FilterType::CatmullRom,
        );
        let isolated = recognize(&enlarged, exe, 6, "txt")?;
        let other = extract_name(&isolated);
        crate::diagnostics::write(
            "OCR_SECOURS",
            &format!(
                "score {score}, titre isolé : {}",
                isolated.trim().chars().take(90).collect::<String>()
            ),
        );
        if matches!(other, Name::Unreadable) {
            candidate
        } else {
            other
        }
    };
    if matches!(verdict, Name::Unreadable) {
        crate::diagnostics::write(
            "OCR_REJET",
            &format!(
                "Nom illisible, score {score}. Texte : {}",
                first.chars().take(130).collect::<String>()
            ),
        );
        return Ok(None);
    }
    // Un passage OCR sur plusieurs lignes peut ajouter des points de suspension
    // imaginaires à un nom pourtant entier. Si cela arrive, relire JUSTE la
    // ligne de titre avec un modèle ligne unique, sans réexaminer l'HDV entier.
    let verdict = if let Name::Truncated(prefix) = &verdict {
        let scale = panel.width as f32 / 413.0;
        let x = (79.0 * scale).round() as u32;
        let y = (41.0 * scale).round() as u32;
        let h = (28.0 * scale).round().max(16.0) as u32;
        let area = CaptureRect {
            x: panel.x.saturating_add(x),
            y: panel.y.saturating_add(y),
            width: panel
                .width
                .saturating_sub(x)
                .saturating_sub((12.0 * scale) as u32),
            height: h,
        };
        let fallback = crate::vision::crop(frame, area).and_then(|crop| {
            let zoom = imageops::resize(
                &crop,
                crop.width() * 3,
                crop.height() * 3,
                imageops::FilterType::CatmullRom,
            );
            recognize(&zoom, exe, 7, "txt").ok()
        });
        match fallback.map(|text| classify(text.trim())) {
            Some(Name::Complete(name)) if normalize(&name).starts_with(&normalize(prefix)) => {
                Name::Complete(name)
            }
            _ => verdict,
        }
    } else {
        verdict
    };
    // IMPORTANT : un long nom complet ne devient pas « tronqué » simplement
    // parce qu'il existe des pixels clairs près du bord droit (icône, police,
    // niveau, etc.). Attendre le survol seulement pour de vrais « ... »/« … »
    // lus dans le titre. Cela évite les attentes injustifiées.
    Ok(Some(verdict))
}

// N'extraire le titre que des lignes correspondant à un nom d'objet.
// Les libellés de l'HDV, les niveaux et les prix ne sont jamais des noms.
fn extract_name(raw: &str) -> Name {
    for line in raw.lines().take(8).map(str::trim) {
        let normalized = normalize(line);
        if normalized.chars().count() < 4
            || [
                "niv",
                "prix",
                "quantit",
                "banque",
                "panoplie",
                "inventaire",
                "havresac",
                "acheter",
                "hoteldevente",
            ]
            .iter()
            .any(|word| normalized.contains(word))
        {
            continue;
        }
        let found = classify(line);
        if !matches!(found, Name::Unreadable) {
            return found;
        }
    }
    Name::Unreadable
}

pub fn classify(raw: &str) -> Name {
    // L'OCR lit parfois une partie de l'icône avant le nom : "A 7 Abracapa".
    let words: Vec<_> = raw.split_whitespace().collect();
    let first = words.iter().position(|part| {
        let candidate = part.trim_matches(|c: char| !c.is_alphabetic());
        let lower = candidate.to_lowercase();
        candidate.chars().filter(|c| c.is_alphabetic()).count() >= 3
            || matches!(
                lower.as_str(),
                "le" | "la" | "de" | "du" | "un" | "au" | "en" | "les" | "des"
            )
    });
    let Some(first) = first else {
        return Name::Unreadable;
    };
    let cleaned = words[first..].join(" ");
    let text = cleaned.trim().to_string();
    if text.chars().filter(|c| c.is_alphabetic()).count() < 4 {
        return Name::Unreadable;
    }
    let tail = text.trim_end();
    let has_ellipsis = tail.ends_with('…')
        || tail.ends_with("...")
        || tail.ends_with("..")
        || tail.ends_with(". . .");
    if has_ellipsis {
        Name::Truncated(
            tail.trim_end_matches(|c: char| c == '.' || c == '…' || c.is_whitespace())
                .to_owned(),
        )
    } else {
        Name::Complete(text)
    }
}

fn normalize(text: &str) -> String {
    text.chars()
        .flat_map(|c| c.to_lowercase())
        .filter_map(|c| {
            let c = match c {
                'à' | 'á' | 'â' | 'ä' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'î' | 'ï' | 'í' => 'i',
                'ô' | 'ö' => 'o',
                'ù' | 'û' | 'ü' => 'u',
                'ç' => 'c',
                'œ' => 'o',
                other => other,
            };
            if c.is_alphanumeric() {
                Some(c)
            } else {
                None
            }
        })
        .collect()
}

#[derive(Default)]
struct Line {
    words: Vec<String>,
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

fn tsv_lines(tsv: &str) -> Vec<(String, CaptureRect)> {
    let mut lines: BTreeMap<(u32, u32, u32), Line> = BTreeMap::new();
    for row in tsv.lines().skip(1) {
        let fields: Vec<_> = row.split('\t').collect();
        if fields.len() < 12 || fields[0] != "5" {
            continue;
        }
        let word = fields[11].trim();
        if word.is_empty() {
            continue;
        }
        let parsed = (
            fields[2].parse::<u32>(),
            fields[3].parse::<u32>(),
            fields[4].parse::<u32>(),
            fields[6].parse::<u32>(),
            fields[7].parse::<u32>(),
            fields[8].parse::<u32>(),
            fields[9].parse::<u32>(),
        );
        let (Ok(block), Ok(par), Ok(line), Ok(x), Ok(y), Ok(w), Ok(h)) = parsed else {
            continue;
        };
        if w == 0 || h == 0 {
            continue;
        }
        let entry = lines.entry((block, par, line)).or_default();
        if entry.words.is_empty() {
            entry.min_x = x;
            entry.min_y = y;
        }
        entry.min_x = entry.min_x.min(x);
        entry.min_y = entry.min_y.min(y);
        entry.max_x = entry.max_x.max(x.saturating_add(w));
        entry.max_y = entry.max_y.max(y.saturating_add(h));
        entry.words.push(word.to_owned());
    }
    lines
        .into_values()
        .filter(|l| !l.words.is_empty())
        .map(|l| {
            (
                l.words.join(" "),
                CaptureRect {
                    x: l.min_x,
                    y: l.min_y,
                    width: l.max_x.saturating_sub(l.min_x),
                    height: l.max_y.saturating_sub(l.min_y),
                },
            )
        })
        .collect()
}

/// Recherche hors de la ligne du titre un texte OCR qui reprend le nom tronqué
/// et le prolonge. La zone de recherche est volontairement bornée autour du titre.
/// En cas de doute, l'application reste en attente au lieu de sauver un faux écran.
pub fn find_hover(
    frame: &RgbaImage,
    panel: CaptureRect,
    prefix: &str,
    exe: &Path,
) -> Result<Option<Hover>, String> {
    let normalized_prefix = normalize(prefix);
    if normalized_prefix.len() < 9 {
        return Ok(None);
    }
    let origin_x = panel.x.saturating_sub(240);
    let origin_y = panel.y.saturating_sub(130);
    let max_w = panel
        .width
        .saturating_add(480)
        .min(frame.width().saturating_sub(origin_x));
    let max_h = 265.min(frame.height().saturating_sub(origin_y));
    let roi = CaptureRect {
        x: origin_x,
        y: origin_y,
        width: max_w,
        height: max_h,
    };
    let Some(source) = crate::vision::crop(frame, roi) else {
        return Ok(None);
    };
    let text = recognize(&source, exe, 11, "tsv")?;
    let mut best = None;
    for (candidate, b) in tsv_lines(&text) {
        let normalized = normalize(&candidate);
        if !normalized.starts_with(&normalized_prefix)
            || normalized.len() < normalized_prefix.len() + 3
        {
            continue;
        }
        if candidate.trim_end().ends_with('…') || candidate.trim_end().ends_with("...") {
            continue;
        }
        let bx = origin_x + b.x;
        let by = origin_y + b.y;
        // Le titre lui-même n'est jamais une preuve de survol.
        if by >= panel.y + 40
            && by <= panel.y + 77
            && bx >= panel.x + 65
            && bx < panel.x + panel.width
        {
            continue;
        }
        let target = CaptureRect {
            x: bx,
            y: by,
            width: b.width,
            height: b.height,
        };
        if best
            .as_ref()
            .is_none_or(|(_, found): &(String, CaptureRect)| found.width < target.width)
        {
            best = Some((candidate, target));
        }
    }
    Ok(best.map(|(name, text_rect)| Hover { name, text_rect }))
}

/// Étend le panneau uniquement pour intégrer le texte complet dans l'infobulle.
/// Ajouter du contexte protège contre un recadrage trop serré autour du texte.
pub fn expanded_rect(panel: CaptureRect, text: CaptureRect, sw: u32, sh: u32) -> CaptureRect {
    let left = panel.x.min(text.x.saturating_sub(28));
    let top = panel.y.min(text.y.saturating_sub(28));
    let right = panel
        .x
        .saturating_add(panel.width)
        .max(text.x.saturating_add(text.width).saturating_add(28))
        .min(sw);
    let bottom = panel
        .y
        .saturating_add(panel.height)
        .max(text.y.saturating_add(text.height).saturating_add(28))
        .min(sh);
    CaptureRect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn portable_runtime_precedes_system_but_keeps_user_override() {
        let app = Path::new("app");
        let custom = Path::new("custom/tesseract.exe");
        let paths = candidates(Some(custom), Some(app), Some(Path::new("programs")));
        assert_eq!(paths[0], custom);
        assert_eq!(paths[1], app.join("ocr/tesseract.exe"));
        assert_eq!(paths[2], Path::new("tesseract"));
        assert_eq!(paths[3], Path::new("programs/Tesseract-OCR/tesseract.exe"));
    }
    #[test]
    fn runtime_always_uses_its_own_language_directory() {
        let root = std::env::temp_dir().join(format!("hdv-ocr-runtime-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tessdata")).unwrap();
        let executable = root.join("tesseract.exe");
        let cmd = command(&executable);
        let arguments = cmd.get_args().collect::<Vec<_>>();
        assert_eq!(arguments[0], "--tessdata-dir");
        assert_eq!(arguments[1], root.join("tessdata"));
        assert!(cmd
            .get_envs()
            .any(|(key, value)| key == "TESSDATA_PREFIX" && value.is_none()));
        assert!(command(Path::new("tesseract")).get_args().next().is_none());
    }
    #[test]
    fn long_complete_name_is_not_truncated() {
        assert_eq!(
            classify("Anneau Cérémonial du Seigneur des Rats"),
            Name::Complete("Anneau Cérémonial du Seigneur des Rats".into())
        );
        assert_eq!(
            classify("Anneau Cérémonial du Seigneur des Rats..."),
            Name::Truncated("Anneau Cérémonial du Seigneur des Rats".into())
        );
    }
    #[test]
    fn standalone_name_survives_metadata_ocr_failure() {
        assert_eq!(
            extract_name("Bottes Phossiles\nNiv. 200\nPr1x moren"),
            Name::Complete("Bottes Phossiles".into())
        );
        assert_eq!(
            extract_name("2 — Cape Rice\n"),
            Name::Complete("Cape Rice".into())
        );
        assert_eq!(
            extract_name("Prix moyen: 12 345\nQuantité en banque: 0"),
            Name::Unreadable
        );
    }
    #[test]
    fn names_truncated_are_not_considered_complete() {
        assert!(matches!(
            classify("Fragment de carte du Chevalier de Glace ..."),
            Name::Truncated(_)
        ));
        assert!(matches!(classify("Ceinture des Pamous"), Name::Complete(_)));
        assert!(matches!(classify("Cape Rice…"), Name::Truncated(_)));
        assert_eq!(
            classify("A 7 Abracapa Ancestrale"),
            Name::Complete("Abracapa Ancestrale".into())
        );
        assert_eq!(
            classify("4 > Cape Rice"),
            Name::Complete("Cape Rice".into())
        );
        assert_eq!(classify("  "), Name::Unreadable);
    }
    #[test]
    fn hover_requires_matching_prefix() {
        let example = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t23\t17\t120\t22\t91\tFragment";
        assert_eq!(tsv_lines(example).len(), 1);
        assert_eq!(normalize("Épée du Glacé"), "epeeduglace");
    }
    #[test]
    fn metadata_survives_a_single_ocr_typo() {
        assert_eq!(
            metadata_score("Cape Rice\nNiv. 32\nPrix moyen: 25 747\nQuantité en banque: 0"),
            3
        );
        assert_eq!(
            metadata_score("Cape Rice\nNiv. 32\nPr1x moren: 25 747\nQuantité en banque: 0"),
            2
        );
        assert_eq!(metadata_score("Objet dans la liste\nPrix moyen: 12 345"), 1);
    }
    #[test]
    fn expansion_covers_tooltip() {
        let r = expanded_rect(
            CaptureRect {
                x: 800,
                y: 100,
                width: 360,
                height: 400,
            },
            CaptureRect {
                x: 640,
                y: 40,
                width: 270,
                height: 18,
            },
            1920,
            1080,
        );
        assert!(r.x <= 640 && r.y <= 40 && r.x + r.width >= 1160);
    }
}
