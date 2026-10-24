//! History persistence, CSV export and session navigation.
use super::*;
#[derive(Clone, Debug)]
pub struct VisitRecord {
    pub id: i64,
    pub name: String,
    pub state: VisitState,
    pub reason: String,
    pub observed_at: String,
    pub evidence: String,
    /// Identifiant de lancement: ne pas lier des objets de deux sessions.
    pub session_id: String,
}
pub fn store_visit(folder: &Path, v: &VisitRecord) -> Result<(), AppError> {
    let db = connect()?;
    db.execute("INSERT INTO visits(id,folder,item_name,state,reason,observed_at,evidence,session_id)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(id) DO UPDATE SET
          item_name=CASE WHEN excluded.item_name <> '' THEN excluded.item_name ELSE visits.item_name END,
          state=excluded.state, reason=excluded.reason,
          session_id=CASE WHEN visits.session_id = '' THEN excluded.session_id ELSE visits.session_id END,
          evidence=CASE WHEN excluded.evidence <> '' THEN excluded.evidence ELSE visits.evidence END",
        params![v.id,folder_key(folder),v.name,v.state,v.reason,v.observed_at,v.evidence,v.session_id])
        .map_err(AppError::from)?;
    Ok(())
}
/// Ne modifie jamais les PNG ni les images de contrôle ; uniquement le suivi SQLite.
pub fn delete_visit(folder: &Path, id: i64) -> Result<usize, AppError> {
    let db = connect()?;
    db.execute(
        "DELETE FROM visits WHERE folder=?1 AND id=?2",
        params![folder_key(folder), id],
    )
    .map_err(AppError::from)
}

/// Select and remove the same rows atomically, returning tombstones for late OCR.
pub fn clear_visits_with_ids(folder: &Path, only_missing: bool) -> Result<Vec<i64>, AppError> {
    let mut db = connect()?;
    clear_visits_on(&mut db, &folder_key(folder), only_missing)
}

fn clear_visits_on(
    db: &mut Connection,
    folder: &str,
    only_missing: bool,
) -> Result<Vec<i64>, AppError> {
    // Reserve the writer before reading ids: a deferred read transaction cannot
    // be upgraded if a concurrent gallery/export write changes its WAL snapshot.
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(AppError::from)?;
    let condition = if only_missing {
        "folder=?1 AND state IN ('a_reprendre','en_attente')"
    } else {
        "folder=?1"
    };
    let ids = {
        let mut statement = tx
            .prepare(&format!("SELECT id FROM visits WHERE {condition}"))
            .map_err(AppError::from)?;
        let rows = statement
            .query_map(params![folder], |row| row.get::<_, i64>(0))
            .map_err(AppError::from)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AppError::from)?
    };
    tx.execute(
        &format!("DELETE FROM visits WHERE {condition}"),
        params![folder],
    )
    .map_err(AppError::from)?;
    tx.commit().map_err(AppError::from)?;
    Ok(ids)
}

pub fn recent_visits(folder: &Path, limit: usize) -> Result<Vec<VisitRecord>, AppError> {
    let db = connect()?;
    let mut stmt = db
        .prepare(
            "SELECT id,item_name,state,reason,observed_at,evidence,session_id FROM visits
                             WHERE folder=?1 ORDER BY id DESC LIMIT ?2",
        )
        .map_err(AppError::from)?;
    let rows = stmt
        .query_map(params![folder_key(folder), limit as i64], |row| {
            Ok(VisitRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                state: row.get(2)?,
                reason: row.get(3)?,
                observed_at: row.get(4)?,
                evidence: row.get(5)?,
                session_id: row.get(6)?,
            })
        })
        .map_err(AppError::from)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
/// Repères de navigation pour une fiche anonyme, sans inventer son identité.
/// `visits` est trié du plus récent au plus ancien (ordre de détection, pas
/// de fin d'OCR). Les noms sont fiables UNIQUEMENT après sauvegarde.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct Neighbors {
    pub before: Option<String>,
    pub after: Option<String>,
    pub ambiguous: usize,
}
pub fn visit_neighbors(visits: &[VisitRecord], index: usize) -> Neighbors {
    let Some(v) = visits.get(index) else {
        return Neighbors::default();
    };
    if v.session_id.is_empty() {
        return Neighbors::default();
    }
    let named = |r: &VisitRecord| {
        r.session_id == v.session_id && r.state == VisitState::Saved && !r.name.trim().is_empty()
    };
    // Les IDs sont attribués à l'ouverture, avant que l'OCR asynchrone finisse.
    let before_idx = (index + 1..visits.len()).find(|&i| named(&visits[i]));
    let after_idx = (0..index).rev().find(|&i| named(&visits[i]));
    let before = before_idx.map(|i| visits[i].name.clone());
    let after = after_idx.map(|i| visits[i].name.clone());
    let ambiguous = match (before_idx, after_idx) {
        (Some(b), Some(a)) if a + 1 < b => visits[a + 1..b]
            .iter()
            .filter(|r| r.session_id == v.session_id && r.state != VisitState::Saved)
            .count(),
        _ => 0,
    };
    Neighbors {
        before,
        after,
        ambiguous,
    }
}

pub fn export_missing(folder: &Path) -> Result<(PathBuf, usize), AppError> {
    let visits = recent_visits(folder, 10000)?;
    let path = folder.join("Objets_a_reprendre.csv");
    let mut text=String::from("\u{FEFF}date;objet;statut;raison;capture_de_controle;objet_avant;objet_apres;fiches_non_confirmees_entre_les_deux\n");
    let mut count = 0;
    for (i, v) in visits.iter().enumerate() {
        if v.state != VisitState::NeedsReview && v.state != VisitState::Pending {
            continue;
        }
        count += 1;
        let neighbors = visit_neighbors(&visits, i);
        let clean = |s: &str| s.replace('\"', "\"\"").replace(['\n', '\r'], " ");
        text.push_str(&format!(
            "\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";\"{}\";{}\n",
            clean(&v.observed_at),
            clean(&v.name),
            clean(v.state.as_str()),
            clean(&v.reason),
            clean(&v.evidence),
            clean(neighbors.before.as_deref().unwrap_or("")),
            clean(neighbors.after.as_deref().unwrap_or("")),
            neighbors.ambiguous
        ));
    }
    fs::create_dir_all(folder).map_err(AppError::from)?;
    fs::write(&path, text).map_err(AppError::from)?;
    Ok((path, count))
}

/// Après un arrêt forcé, aucune lecture d'une session précédente ne doit
/// rester indéfiniment affichée comme « en cours ».
pub fn recover_interrupted(folder: &Path) -> Result<usize, AppError> {
    let db = connect()?;
    let n = db
        .execute(
            "UPDATE visits SET state='a_reprendre',
        reason='Capture interrompue avant confirmation' WHERE folder=?1 AND state='en_attente'",
            params![folder_key(folder)],
        )
        .map_err(AppError::from)?;
    Ok(n)
}

#[cfg(test)]
mod neighbor_tests {
    use super::*;
    fn v(id: i64, name: &str, state: &str, session: &str) -> VisitRecord {
        VisitRecord {
            id,
            name: name.into(),
            state: state.into(),
            reason: String::new(),
            observed_at: String::new(),
            evidence: String::new(),
            session_id: session.into(),
        }
    }
    #[test]
    fn anonymous_gap_is_framed_by_known_items_not_guessed() {
        let visits = vec![
            v(5, "Après", "enregistre", "A"),
            v(4, "", "a_reprendre", "A"),
            v(3, "", "a_reprendre", "A"),
            v(2, "Avant", "enregistre", "A"),
        ];
        let c = visit_neighbors(&visits, 1);
        assert_eq!(c.before.as_deref(), Some("Avant"));
        assert_eq!(c.after.as_deref(), Some("Après"));
        assert_eq!(c.ambiguous, 2);
    }
    #[test]
    fn sessions_are_not_connected_even_when_ordered() {
        let visits = vec![
            v(5, "Après", "enregistre", "B"),
            v(4, "", "a_reprendre", "B"),
            v(3, "Avant", "enregistre", "A"),
        ];
        let c = visit_neighbors(&visits, 1);
        assert!(c.before.is_none());
        assert_eq!(c.after.as_deref(), Some("Après"));
    }
    #[test]
    fn incomplete_names_and_legacy_without_session_are_not_used() {
        let visits = vec![
            v(3, "Après", "enregistre", ""),
            v(2, "", "a_reprendre", ""),
            v(1, "Avant", "enregistre", ""),
        ];
        assert_eq!(visit_neighbors(&visits, 1), Neighbors::default());
    }
}

#[cfg(test)]
mod deletion_tests {
    use super::*;
    #[test]
    fn deletion_scopes_never_touch_items_or_other_folders() {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE visits(id INTEGER PRIMARY KEY,folder TEXT,state TEXT);
            CREATE TABLE items(path TEXT PRIMARY KEY);
            INSERT INTO visits VALUES(1,'A','enregistre'),(2,'A','a_reprendre'),
                (3,'A','en_attente'),(4,'B','a_reprendre');
            INSERT INTO items VALUES('capture.png');",
        )
        .unwrap();
        let mut removed = clear_visits_on(&mut db, "A", true).unwrap();
        removed.sort_unstable();
        assert_eq!(removed, vec![2, 3]);
        assert_eq!(
            db.query_row("SELECT count(*) FROM visits", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM items", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        // A new observation after clearing must remain independently removable.
        db.execute("INSERT INTO visits VALUES(5,'A','en_attente')", [])
            .unwrap();
        let mut removed = clear_visits_on(&mut db, "A", false).unwrap();
        removed.sort_unstable();
        assert_eq!(removed, vec![1, 5]);
        assert_eq!(
            db.query_row("SELECT id FROM visits", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            4
        );
    }
}
