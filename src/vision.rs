//! Détection purement visuelle du panneau de détail HDV Dofus 3.
//! La région calibrée est une INDICATION de recherche : les bords du rectangle
//! tracé par l'utilisateur ne sont jamais supposés être ceux du panneau.
//! Aucune lecture de mémoire ni interaction avec le processus du jeu.
use crate::settings::CaptureRect;
use image::{imageops::crop_imm, RgbaImage};

#[derive(Clone, Debug)]
pub struct Detection {
    pub rect: CaptureRect,
}

fn rgb(pixels: &[u8], image_width: u32, x: u32, y: u32) -> &[u8] {
    let offset = (y as usize * image_width as usize + x as usize) * 4;
    &pixels[offset..offset + 3]
}

// Barre supérieure uniforme du panneau de détail HDV. Reconnaissance avec
// tolérance de couleur, sans gabarit dépendant de la position ni de la hauteur.
fn header_pixel(c: &[u8]) -> bool {
    (c[0] as i32 - 84).abs() <= 15
        && (c[1] as i32 - 84).abs() <= 15
        && (c[2] as i32 - 114).abs() <= 19
}

fn border_pixel(c: &[u8]) -> bool {
    let r = c[0] as i32;
    let g = c[1] as i32;
    let b = c[2] as i32;
    // Bord latéral du panneau : gris ardoise (environ 55,57,74).
    (r - 55).abs() <= 13 && (g - 57).abs() <= 14 && (b - 74).abs() <= 17
}

fn dark_blue(c: &[u8]) -> bool {
    c[0] >= 22
        && c[0] <= 85
        && c[1] >= 23
        && c[1] <= 95
        && c[2] >= 43
        && c[2] <= 115
        && c[2] >= c[0].saturating_add(8)
}

pub fn default_region(screen_width: u32, screen_height: u32) -> CaptureRect {
    // Centre approximatif issu des exemples. La détection cherche réellement
    // les bordures du panneau même si le rectangle est décalé.
    CaptureRect {
        x: (screen_width as f32 * 0.4605) as u32,
        y: (screen_height as f32 * 0.0935) as u32,
        width: (screen_width as f32 * 0.218) as u32,
        height: (screen_height as f32 * 0.76) as u32,
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    rect: CaptureRect,
    rank: i64,
}

// Une fiche de prix n'a PAS de titre dans sa barre supérieure. Les fenêtres
// "Hôtel de vente" (liste) et "Inventaire" ont au contraire un titre blanc.
fn empty_title_bar(frame: &RgbaImage, left: u32, top: u32, width: u32) -> bool {
    let (sw, sh) = frame.dimensions();
    if left.saturating_add(width) > sw || top.saturating_add(32) > sh || width < 240 {
        return false;
    }
    let data = frame.as_raw();
    let mut bright = 0u32;
    for y in ((top + 7)..(top + 29)).step_by(2) {
        for x in ((left + 25)..(left + width - 35)).step_by(2) {
            let c = rgb(data, sw, x, y);
            if c[0] > 165 && c[1] > 165 && c[2] > 165 {
                bright += 1;
                if bright > 12 {
                    return false;
                }
            }
        }
    }
    true
}

// Couleur de la colonne des boutons ACHETER dans les lignes de prix.
// Sur les captures fournies elle se répète à intervalles réguliers, alors
// que la liste d'objets et l'inventaire n'ont pas cette structure.
fn buy_button_background(c: &[u8]) -> bool {
    // Bouton bleu habituel OU bouton grisé (vente non accessible).
    // Dans les deux cas la validation exige l'entête de fiche et un
    // motif répété de lignes ACHETER, jamais la seule couleur du fond.
    (c[0] >= 55
        && c[0] <= 98
        && c[1] >= 55
        && c[1] <= 103
        && c[2] >= 85
        && c[2] <= 145
        && c[2] >= c[0].saturating_add(19))
        || (c[0] >= 68
            && c[0] <= 104
            && c[1].abs_diff(c[0]) <= 5
            && c[2] >= c[0].saturating_add(4)
            && c[2] <= c[0].saturating_add(13))
}

// Cherche plusieurs lignes de vente après l'en-tête "Prix moyen".
// Le dernier bouton donne une borne inférieure au bas du panneau : il est
// INTERDIT de recadrer au-dessus de cette borne.
fn price_rows(
    frame: &RgbaImage,
    left: u32,
    top: u32,
    right: u32,
    last_border: u32,
) -> Option<(usize, u32)> {
    let (sw, sh) = frame.dimensions();
    let width = right.checked_sub(left)?;
    let scale = width as f32 / 413.0;
    let start = top.saturating_add((185.0 * scale).round() as u32);
    // Regarder sous le bas présumé : une bordure interrompue ne doit pas
    // masquer des offres encore affichées plus bas.
    let stop = last_border
        .saturating_add((240.0 * scale).round() as u32)
        .min(sh);
    if stop <= start {
        return None;
    }
    let data = frame.as_raw();
    let mut best: Option<(usize, u32)> = None;

    // Selon le nombre affiché dans le bouton, certaines colonnes traversent
    // les lettres claires d'ACHETER : sonder des voisins est plus robuste.
    for offset in [64.0_f32, 65.0, 66.0, 67.0, 68.0, 70.0] {
        let Some(x) = right.checked_sub((offset * scale).round() as u32) else {
            continue;
        };
        if x >= sw {
            continue;
        }
        let mut spans = Vec::new();
        let mut y = start;
        while y < stop {
            if !buy_button_background(rgb(data, sw, x, y)) {
                y += 1;
                continue;
            }
            let beginning = y;
            while y < stop && buy_button_background(rgb(data, sw, x, y)) {
                y += 1;
            }
            let height = y - beginning;
            if height >= (7.0 * scale).round().max(5.0) as u32
                && height <= (31.0 * scale).round().max(10.0) as u32
            {
                spans.push((beginning, y - 1));
            }
        }
        if spans.is_empty() {
            continue;
        }
        // Plusieurs lignes : leurs espacements doivent être cohérents.
        if spans.len() > 1 {
            let regular = spans
                .windows(2)
                .filter(|p| {
                    let distance = p[1].0.saturating_sub(p[0].0);
                    distance >= (27.0 * scale).round() as u32
                        && distance <= (55.0 * scale).round() as u32
                })
                .count();
            if regular + 1 < spans.len().saturating_sub(1).max(1) {
                continue;
            }
        }
        let Some(&(_, last_y)) = spans.last() else {
            continue;
        };
        if best.is_none_or(|(old, _)| spans.len() > old) {
            best = Some((spans.len(), last_y));
        }
    }
    best
}

// Validation POSITIVE d'une véritable fiche de détail, en coordonnées LOCALES.
// Contrairement à la liste générale, elle possède une barre grise sans titre,
// une zone nom+niveau sous la barre, puis une colonne de boutons ACHETER.
// Ce contrôle est également appliqué juste avant l'OCR et la sauvegarde :
// une erreur de détection ne peut plus devenir un fichier PNG mal nommé.
fn real_detail_header(frame: &RgbaImage, left: u32, top: u32, width: u32) -> bool {
    let (sw, sh) = frame.dimensions();
    if !(300..=650).contains(&width) || left.saturating_add(width) > sw {
        return false;
    }
    let scale = width as f32 / 411.0;
    let at_y = top.saturating_add((17.0 * scale).round() as u32);
    if at_y >= sh {
        return false;
    }
    let bytes = frame.as_raw();
    // La barre de la fiche est (82,83,113) sur les images réelles.
    // Les barres de la liste sont visiblement plus bleutées (70,74,111).
    // Échantillonner plusieurs points pour éviter le bouton X et les reflets.
    let mut matched = 0;
    for fraction in [0.23_f32, 0.34, 0.45, 0.55, 0.65, 0.76] {
        let x = left + ((width as f32 * fraction).round() as u32);
        let c = rgb(bytes, sw, x, at_y);
        if (76..=96).contains(&c[0])
            && (76..=97).contains(&c[1])
            && (105..=130).contains(&c[2])
            && c[0].abs_diff(c[1]) <= 6
        {
            matched += 1;
        }
    }
    if matched < 5 {
        return false;
    }
    // Un nom visible, et non une fenêtre vide ou une barre de catégorie.
    let mut white = 0u32;
    let x0 = left + (width as f32 * 0.21) as u32;
    let x1 = left + (width as f32 * 0.78) as u32;
    let y0 = top.saturating_add((48.0 * scale) as u32);
    let y1 = top.saturating_add((72.0 * scale) as u32).min(sh);
    for y in (y0..y1).step_by(2) {
        for x in (x0..x1.min(sw)).step_by(2) {
            let c = rgb(bytes, sw, x, y);
            if c[0] > 174 && c[1] > 174 && c[2] > 174 {
                white += 1;
            }
        }
    }
    white >= 8
}

// Repérage renforcé de vrais boutons ACHETER : la liste générale HDV ne
// possède PAS cette colonne, même si sa colonne de prix comporte du texte
// blanc et des rectangles violets. Le rectangle d'un bouton est reconnaissable
// sur deux positions horizontales et un motif de texte clair au centre.
// Sonde une colonne du bouton ACHETER. Le décalage du bord à cette
// colonne varie légèrement selon le cadrage et la mise à l'échelle.
fn real_buy_rows_at(
    frame: &RgbaImage,
    left: u32,
    top: u32,
    width: u32,
    bottom: u32,
    offset_px: f32,
) -> usize {
    let (sw, sh) = frame.dimensions();
    if width < 300 || left.saturating_add(width) > sw || bottom > sh {
        return 0;
    }
    let scale = width as f32 / 411.0;
    let offset = (offset_px * scale).round() as u32;
    let Some(x) = left.checked_add(width).and_then(|r| r.checked_sub(offset)) else {
        return 0;
    };
    let start = top.saturating_add((195.0 * scale) as u32);
    let end = bottom.saturating_sub((8.0 * scale).round() as u32);
    if end <= start {
        return 0;
    }
    let bytes = frame.as_raw();
    let mut spans = Vec::new();
    let mut y = start;
    while y < end {
        if !buy_button_background(rgb(bytes, sw, x, y)) {
            y += 1;
            continue;
        }
        let first = y;
        while y < end && buy_button_background(rgb(bytes, sw, x, y)) {
            y += 1;
        }
        let height = y - first;
        if height >= (7.0 * scale).round().max(5.0) as u32
            && height <= (23.0 * scale).round().max(10.0) as u32
        {
            let middle = (first + y) / 2;
            // Chercher au moins quelques points de texte blanc ACHETER sur
            // la largeur du bouton (un simple fond uni n'est pas suffisant).
            let xr = left + width;
            let from = xr.saturating_sub((88.0 * scale).round() as u32);
            let to = xr.saturating_sub((18.0 * scale).round() as u32);
            let mut glyphs = 0;
            for yy in middle.saturating_sub(3)..=middle.saturating_add(3).min(sh - 1) {
                for xx in (from..to).step_by(2) {
                    let c = rgb(bytes, sw, xx, yy);
                    if c[0] > 110 && c[1] > 110 && c[2] > 115 {
                        glyphs += 1;
                    }
                }
            }
            if glyphs >= 6 {
                spans.push(first);
            }
        }
    }
    // Fusionner les bandes d'un même bouton, puis confirmer les espacements.
    let mut separate = Vec::new();
    for y in spans {
        if separate
            .last()
            .is_none_or(|&prev| y - prev > (24.0 * scale) as u32)
        {
            separate.push(y);
        }
    }
    if separate.len() >= 2 {
        let near = separate
            .windows(2)
            .filter(|p| {
                let d = p[1] - p[0];
                d >= (29.0 * scale) as u32 && d <= (51.0 * scale) as u32
            })
            .count();
        if near == 0 {
            return 0;
        }
    }
    separate.len()
}

// Une seule colonne fixe (=55 px) rejetait des vraies fiches : sur les
// captures Dofus plein écran, elle coupe les lettres d'ACHETER, tandis que
// sur les recadrages plus petits elle tombe bien dans le fond du bouton.
// Choisir le meilleur alignement parmi plusieurs colonnes reste exigeant :
// chacune doit contenir du fond coloré, du texte clair et, pour plusieurs
// offres, un espacement régulier. L'en-tête est vérifié indépendamment.
fn real_buy_rows(frame: &RgbaImage, left: u32, top: u32, width: u32, bottom: u32) -> usize {
    [55.0_f32, 59.0, 62.0, 66.0, 67.0, 68.0, 70.0]
        .into_iter()
        .map(|offset| real_buy_rows_at(frame, left, top, width, bottom, offset))
        .max()
        .unwrap_or(0)
}

/// Refuse les faux panneaux AVANT l'OCR, y compris en capture manuelle.
/// Un unique vrai bouton est permis pour un objet très peu vendu ; les
/// autres indices structurels restent obligatoires.
pub fn verified_detail_crop(panel: &RgbaImage) -> bool {
    let (w, h) = panel.dimensions();
    if h < 215 || !real_detail_header(panel, 0, 0, w) {
        return false;
    }
    real_buy_rows(panel, 0, 0, w, h) >= 1
}

fn candidate_at(
    frame: &RgbaImage,
    run_x: u32,
    run_y: u32,
    run_width: u32,
    hint: CaptureRect,
) -> Option<Candidate> {
    let (sw, sh) = frame.dimensions();
    let data = frame.as_raw();
    let center_x = run_x + run_width / 2;

    // Le texte blanc des titres de fenêtres (notamment « Hôtel de vente »)
    // coupe la bande colorée AU CENTRE. Un seul échantillon central
    // pouvait donc placer à tort le « haut » sous le texte, ce qui
    // faisait passer la liste HDV pour une fiche de prix.
    // Deux échantillons latéraux + le centre remontent jusqu'au vrai haut.
    let anchor_xs = [
        run_x.saturating_add(24).min(sw - 1),
        center_x.min(sw - 1),
        run_x
            .saturating_add(run_width)
            .saturating_sub(55)
            .min(sw - 1),
    ];
    let mut top = run_y;
    while top > 0 && run_y - top < 55 {
        let above = top - 1;
        let matches = anchor_xs
            .iter()
            .filter(|&&x| header_pixel(rgb(data, sw, x, above)))
            .count();
        if matches < 2 {
            break;
        }
        top -= 1;
    }
    let left_limit = run_x.saturating_sub(10);
    let mut best_border: Option<(u32, usize)> = None;
    for x in left_limit..=run_x {
        let mut score = 0usize;
        for dy in (50..190).step_by(9) {
            let y = top.saturating_add(dy);
            if y < sh && border_pixel(rgb(data, sw, x, y)) {
                score += 1;
            }
        }
        if best_border.is_none_or(|(_, old)| score > old) {
            best_border = Some((x, score));
        }
    }
    let (left, border_strength) = best_border?;
    if border_strength < 10 {
        return None;
    }

    let right = (run_x + run_width + 2).min(sw);
    let panel_width = right.saturating_sub(left);
    if !(240..=850).contains(&panel_width) || top + 220 >= sh {
        return None;
    }

    // NE PAS confondre avec la grande liste HDV (qui porte un titre) ou
    // l'inventaire : contrairement au panneau de détail, leurs barres sont
    // remplies de texte blanc.
    if !empty_title_bar(frame, left, top, panel_width) {
        return None;
    }

    let mut interior = 0;
    for dy in [75, 135, 185] {
        for offset in [23, panel_width / 3, panel_width.saturating_sub(23)] {
            let x = left + offset;
            if x < sw && dark_blue(rgb(data, sw, x, top + dy)) {
                interior += 1;
            }
        }
    }
    if interior < 5 {
        return None;
    }

    // Suivre la bordure gauche avec une tolérance aux coupures graphiques,
    // mais NE JAMAIS accepter une image tronquée en bas de l'écran.
    let mut misses = 0;
    let mut last_border = top + 175;
    let mut ended = false;
    for y in (top + 175)..sh {
        if border_pixel(rgb(data, sw, left, y)) {
            misses = 0;
            last_border = y;
        } else {
            misses += 1;
            if misses >= 42 {
                ended = true;
                break;
            }
        }
    }
    if !ended || last_border <= top + 200 {
        return None;
    }

    // Preuve positive : les lignes de vente et leurs boutons ACHETER, avec
    // un dernier bouton entièrement au-dessus du bas de la fenêtre.
    let (rows, last_row_end) = price_rows(frame, left, top, right, last_border)?;
    if last_border < last_row_end.saturating_add(5) || last_border > last_row_end.saturating_add(85)
    {
        return None;
    }

    let crop_top = top.saturating_sub(3);
    let bottom = last_border.saturating_add(10).min(sh);
    if bottom.saturating_sub(crop_top) < 210 {
        return None;
    }
    let rect = CaptureRect {
        x: left,
        y: crop_top,
        width: panel_width,
        height: bottom.saturating_sub(crop_top),
    };
    // Ne pas classer la liste générale sur ses seules couleurs et bordures.
    let validated = crop(frame, rect)?;
    if !verified_detail_crop(&validated) {
        return None;
    }

    let hint_cx = hint.x.saturating_add(hint.width / 2) as i64;
    let hint_cy = hint.y.saturating_add(70) as i64;
    let candidate_cx = rect.x.saturating_add(rect.width / 2) as i64;
    let candidate_cy = rect.y.saturating_add(70) as i64;
    let dx = (candidate_cx - hint_cx).abs();
    let dy = (candidate_cy - hint_cy).abs();
    let rank = border_strength as i64 * 60 + interior as i64 * 45 + rows as i64 * 100
        - dx / 3
        - dy / 3
        - (panel_width as i64 - hint.width as i64).abs() / 8;
    Some(Candidate { rect, rank })
}

/// Recherche la barre supérieure et la bordure verticale caractéristiques du
/// panneau, sur l'écran sélectionné. La calibration n'est qu'un repère :
/// une marge de plusieurs dizaines/centaines de pixels est tolérée.
/// Si aucune fenêtre suffisamment sûre n'est visible, ne rien capturer.
/// Cherche un panneau dans une sous-zone SANS recopier l'écran.
/// Les coordonnées des candidats restent celles de l'écran entier.
fn detect_in(
    frame: &RgbaImage,
    calibrated: CaptureRect,
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
) -> Option<Detection> {
    let (sw, sh) = frame.dimensions();
    if sw < 320 || sh < 350 {
        return None;
    }
    let pixels = frame.as_raw();
    let mut best: Option<Candidate> = None;
    let x_start = left.min(sw);
    let x_end = right.min(sw);
    let y_start = top.max(18).min(sh.saturating_sub(220));
    let y_end = bottom.min(sh.saturating_sub(220));
    if x_end <= x_start || y_end <= y_start {
        return None;
    }
    for y in (y_start..y_end).step_by(3) {
        let mut x = x_start;
        while x < x_end {
            if !header_pixel(rgb(pixels, sw, x, y)) {
                x += 1;
                continue;
            }
            let start = x;
            x += 1;
            while x < x_end && header_pixel(rgb(pixels, sw, x, y)) {
                x += 1;
            }
            let len = x - start;
            if !(225..=850).contains(&len) {
                continue;
            }
            if let Some(found) = candidate_at(frame, start, y, len, calibrated) {
                if best.is_none_or(|prev| found.rank > prev.rank) {
                    best = Some(found);
                }
            }
        }
    }
    best.map(|candidate| Detection {
        rect: candidate.rect,
    })
}

/// Premier passage rapide autour du repère ou de la dernière fiche reconnue.
/// Une grande marge permet de déplacer la fiche ou de dessiner un repère vague.
pub fn detect_near(frame: &RgbaImage, hint: CaptureRect) -> Option<Detection> {
    let (sw, sh) = frame.dimensions();
    let left = hint.x.saturating_sub(440);
    let right = hint
        .x
        .saturating_add(hint.width)
        .saturating_add(440)
        .min(sw);
    let top = hint.y.saturating_sub(175);
    let bottom = hint.y.saturating_add(330).min(sh);
    detect_in(frame, hint, left, top, right, bottom)
}

/// Recherche de secours sur tout l'écran (utilisée ponctuellement).
pub fn detect(frame: &RgbaImage, calibrated: CaptureRect) -> Option<Detection> {
    detect_in(frame, calibrated, 0, 18, frame.width(), frame.height())
}

/// Avant une capture avec infobulle, confirme que les lignes de prix sont
/// toujours visibles au même emplacement. Évite de sauvegarder une ancienne
/// sélection lorsque l'overlay interrompt la détection des bordures.
pub fn offers_still_visible(frame: &RgbaImage, panel: CaptureRect) -> bool {
    if !crop(frame, panel).is_some_and(|part| verified_detail_crop(&part)) {
        return false;
    }
    let top = panel.y.saturating_add(3);
    let right = panel.x.saturating_add(panel.width);
    let bottom = panel.y.saturating_add(panel.height);
    let boundary = bottom.saturating_sub(10);
    match price_rows(frame, panel.x, top, right, boundary) {
        Some((count, last)) => count > 0 && last.saturating_add(5) <= boundary,
        None => false,
    }
}

/// Recadrage générique, y compris les petites zones OCR de 27 pixels.
/// L'ancienne limite arbitraire 100×100 rendait toute lecture de titre
/// impossible et affichait à tort « Titre HDV hors écran ».
pub fn crop(frame: &RgbaImage, rect: CaptureRect) -> Option<RgbaImage> {
    if rect.x >= frame.width() || rect.y >= frame.height() {
        return None;
    }
    let w = rect.width.min(frame.width() - rect.x);
    let h = rect.height.min(frame.height() - rect.y);
    if w < 10 || h < 10 {
        return None;
    }
    Some(crop_imm(frame, rect.x, rect.y, w, h).to_image())
}

#[derive(Clone, Debug)]
pub struct Fingerprint {
    values: Vec<u8>,
    width: u32,
    height: u32,
}

/// Miniature de comparaison des prix, évitant de re-capturer la même fiche.
pub fn fingerprint(img: &RgbaImage) -> Fingerprint {
    let mut values = Vec::with_capacity(112 * 112 * 3);
    for y in 0..112 {
        for x in 0..112 {
            let px = img.get_pixel(x * img.width() / 112, y * img.height() / 112);
            for i in 0..3 {
                values.push(px[i] / 4);
            }
        }
    }
    Fingerprint {
        values,
        width: img.width(),
        height: img.height(),
    }
}

/// Empreinte du titre de la fiche, plus discriminante que le panneau entier.
/// Évite de rater deux objets aux fenêtres similaires mais aux noms différents.
pub fn title_fingerprint(panel: &RgbaImage) -> Fingerprint {
    let x = panel.width().min(74);
    let y = panel.height().min(43);
    let width = panel.width().saturating_sub(x).clamp(1, 320);
    let height = panel.height().saturating_sub(y).clamp(1, 47);
    let title = image::imageops::crop_imm(panel, x, y, width, height).to_image();
    fingerprint(&title)
}

pub fn same_title(a: &Fingerprint, b: &Fingerprint) -> bool {
    if a.width.abs_diff(b.width) > 5 || a.height.abs_diff(b.height) > 3 {
        return false;
    }
    let changed = a
        .values
        .iter()
        .zip(b.values.iter())
        .filter(|(x, y)| (**x).abs_diff(**y) > 6)
        .count();
    // 1.5% de canaux différents suffit à distinguer deux titres ;
    // un changement de nom/icone doit relancer l'OCR.
    changed * 1000 < a.values.len() * 15
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fake_panel() -> RgbaImage {
        let mut canvas = RgbaImage::from_pixel(1400, 800, image::Rgba([210, 182, 110, 255]));
        let left = 625u32;
        let top = 85u32;
        let right = 1038u32;
        let bottom = 585u32;
        for y in top..bottom {
            for x in left..right {
                let value = if y < top + 34 {
                    [84, 84, 114, 255]
                } else {
                    [35, 37, 57, 255]
                };
                canvas.put_pixel(x, y, image::Rgba(value));
            }
            if y >= top + 34 {
                canvas.put_pixel(left, y, image::Rgba([55, 57, 74, 255]));
            }
        }
        // Une vraie fiche a un titre lisible sous la barre et une colonne
        // répétée de boutons avec leur libellé blanc.
        for y in top + 50..top + 59 {
            for x in left + 90..left + 145 {
                canvas.put_pixel(x, y, image::Rgba([221, 223, 228, 255]));
            }
        }
        for row in 0..7 {
            let sy = top + 225 + row * 38;
            for y in sy..sy + 14 {
                for x in right - 84..right - 16 {
                    canvas.put_pixel(x, y, image::Rgba([73, 75, 108, 255]));
                }
            }
            for y in sy + 6..sy + 9 {
                for x in right - 84..right - 72 {
                    canvas.put_pixel(x, y, image::Rgba([219, 220, 227, 255]));
                }
            }
        }
        canvas
    }
    #[test]
    fn detects_panel_with_imprecise_calibration() {
        let canvas = fake_panel();
        for r in [
            CaptureRect {
                x: 570,
                y: 40,
                width: 570,
                height: 650,
            },
            CaptureRect {
                x: 680,
                y: 115,
                width: 300,
                height: 350,
            },
            CaptureRect {
                x: 550,
                y: 150,
                width: 650,
                height: 200,
            },
        ] {
            let rect = detect(&canvas, r)
                .expect("Le panneau doit être reconnu")
                .rect;
            assert!(rect.x.abs_diff(625) < 10);
            assert!(rect.y.abs_diff(85) < 10);
            assert!(rect.width.abs_diff(413) < 12);
            assert!(rect.height.abs_diff(500) < 18);
        }
    }
    #[test]
    fn refuses_item_list_even_with_matching_window_border() {
        let mut canvas = fake_panel();
        // Titre blanc de la liste : impossible de le confondre avec la fiche.
        for y in 93..113 {
            for x in 795..865 {
                canvas.put_pixel(x, y, image::Rgba([240, 240, 240, 255]));
            }
        }
        assert!(detect(
            &canvas,
            CaptureRect {
                x: 625,
                y: 85,
                width: 413,
                height: 500
            }
        )
        .is_none());
    }
    #[test]
    fn refuses_panel_with_truncated_prices() {
        let mut canvas = fake_panel();
        // Un écran qui s'arrête avant les dernières offres ne doit jamais
        // fournir un PNG prétendument complet.
        for y in 445..585 {
            canvas.put_pixel(625, y, image::Rgba([20, 20, 20, 255]));
        }
        assert!(detect(
            &canvas,
            CaptureRect {
                x: 625,
                y: 85,
                width: 413,
                height: 500
            }
        )
        .is_none());
    }
    #[test]
    fn rejects_blank_screens() {
        let frame = RgbaImage::new(1920, 1080);
        assert!(detect(&frame, default_region(1920, 1080)).is_none());
    }
    #[test]
    fn accepts_short_ocr_crop() {
        let canvas = fake_panel();
        let text = crop(
            &canvas,
            CaptureRect {
                x: 700,
                y: 140,
                width: 310,
                height: 27,
            },
        );
        assert_eq!(text.unwrap().height(), 27);
    }
    #[test]
    fn title_fingerprints_detect_large_changes() {
        let a = RgbaImage::from_pixel(200, 200, image::Rgba([10, 20, 30, 255]));
        let b = RgbaImage::from_pixel(200, 200, image::Rgba([200, 20, 30, 255]));
        let original = title_fingerprint(&a);
        let changed = title_fingerprint(&b);
        assert!(same_title(&original, &original));
        assert!(!same_title(&original, &changed));
    }
}

#[cfg(test)]
mod regression_exemples {
    use super::*;
    #[test]
    fn recognizes_change_of_title_without_full_screen_ocr() {
        let a = RgbaImage::from_pixel(413, 440, image::Rgba([39, 41, 61, 255]));
        let mut b = a.clone();
        for y in 49..69 {
            for x in 100..190 {
                b.put_pixel(x, y, image::Rgba([245, 245, 245, 255]));
            }
        }
        assert!(!same_title(&title_fingerprint(&a), &title_fingerprint(&b)));
        assert!(same_title(&title_fingerprint(&a), &title_fingerprint(&a)));
    }
}
