//! Serialized history writes and late-event tombstones.
use super::*;
// Le scan des fichiers et les lectures SQLite ne sont jamais effectues
// depuis le fil graphique (surtout apres une rafale de captures).
// Toutes les mutations de l'historique passent par une seule file FIFO.
// Une suppression ne peut donc pas être annulée par une écriture SQLite déjà en attente.
pub(super) enum VisitWrite {
    Store(PathBuf, index::VisitRecord),
    Delete(PathBuf, i64, Sender<Result<Vec<i64>, AppError>>),
    ClearMissing(PathBuf, Sender<Result<Vec<i64>, AppError>>),
    ClearAll(PathBuf, Sender<Result<Vec<i64>, AppError>>),
    Flush(Sender<()>),
    Evidence(PathBuf, i64, PathBuf),
}

#[derive(Clone)]
pub(super) enum HistoryMutation {
    Delete(i64),
    ClearMissing,
    ClearAll,
}

pub(super) fn run_visit_writer(visit_rx: Receiver<VisitWrite>) {
    let mut dismissed: HashMap<PathBuf, HashSet<i64>> = HashMap::new();
    while let Ok(cmd) = visit_rx.recv() {
        match cmd {
            VisitWrite::Flush(ack) => {
                let _ = ack.send(());
            }
            VisitWrite::Evidence(folder, id, path) => {
                if let Err(e) = index::attach_evidence(&folder, id, &path) {
                    diagnostics::write("SUIVI", &e);
                }
            }
            VisitWrite::Store(folder, record) => {
                if !dismissed
                    .get(&folder)
                    .is_some_and(|ids| ids.contains(&record.id))
                {
                    if let Err(e) = index::store_visit(&folder, &record) {
                        diagnostics::write("SUIVI", &e);
                    }
                }
            }
            VisitWrite::Delete(folder, id, ack) => {
                let result = index::delete_visit(&folder, id);
                if result.is_ok() {
                    dismissed.entry(folder).or_default().insert(id);
                }
                let _ = ack.send(result.map(|_| vec![id]));
            }
            VisitWrite::ClearMissing(folder, ack) => {
                let result = index::clear_visits_with_ids(&folder, true);
                let result = result.inspect(|ids| {
                    dismissed
                        .entry(folder)
                        .or_default()
                        .extend(ids.iter().copied());
                });
                let _ = ack.send(result);
            }
            VisitWrite::ClearAll(folder, ack) => {
                let result = index::clear_visits_with_ids(&folder, false);
                let result = result.inspect(|ids| {
                    dismissed
                        .entry(folder)
                        .or_default()
                        .extend(ids.iter().copied());
                });
                let _ = ack.send(result);
            }
        }
    }
}
