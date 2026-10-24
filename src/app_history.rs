//! History actions and confirmation dialogs.
use super::*;
impl App {
    pub(super) fn mutate_history(&mut self, change: HistoryMutation) {
        if self.history_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let cmd = match &change {
            HistoryMutation::Delete(id) => {
                VisitWrite::Delete(self.settings.destination.clone(), *id, tx)
            }
            HistoryMutation::ClearMissing => {
                VisitWrite::ClearMissing(self.settings.destination.clone(), tx)
            }
            HistoryMutation::ClearAll => {
                VisitWrite::ClearAll(self.settings.destination.clone(), tx)
            }
        };
        if self.visit_tx.send(cmd).is_ok() {
            self.history_rx = Some((change, rx));
        } else {
            self.report_error("Impossible de contacter le gestionnaire de l'historique.");
        }
    }

    pub(super) fn check_history_mutation(&mut self) {
        let result = self.history_rx.as_ref().map(|(_, rx)| rx.try_recv());
        match result {
            Some(Ok(Ok(ids))) => {
                self.history_rx.take();
                let count = ids.len();
                self.dismissed_visit_ids.extend(ids);
                self.visits
                    .retain(|v| !self.dismissed_visit_ids.contains(&v.id));
                if self
                    .selected_visit
                    .as_ref()
                    .is_some_and(|v| self.dismissed_visit_ids.contains(&v.id))
                {
                    self.selected_visit = None;
                    self.selected_texture = None;
                    self.selected_path = None;
                }
                self.notice=format!("Historique mis à jour : {count} entrée(s) retirée(s). Les PNG restent intacts.");
                self.notice_is_error = false;
            }
            Some(Ok(Err(e))) => {
                self.history_rx = None;
                self.report_error(format!("Modification de l'historique impossible : {e}"));
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.history_rx = None;
                self.report_error("Le gestionnaire de l'historique s'est interrompu.");
            }
            _ => {}
        }
    }

    pub(super) fn history_confirmation(&mut self, ctx: &egui::Context) {
        if self.confirm_delete_visit.is_none() && self.confirm_clear_history.is_none() {
            return;
        }
        let is_delete = self.confirm_delete_visit.is_some();
        let delete_all = self.confirm_clear_history == Some(true);
        let title = if is_delete {
            "Retirer une fiche du suivi"
        } else if delete_all {
            "Réinitialiser l'historique de collecte"
        } else {
            "Vider la liste à reprendre"
        };
        let mut open = true;
        let mut do_confirm = false;
        let mut cancel = false;
        egui::Window::new(title).collapsible(false).resizable(false)
            .open(&mut open).show(ctx,|ui|{
                ui.label(if is_delete {
                    "Cette entrée sera retirée de l'historique."
                } else if delete_all {
                    "Toutes les entrées de l'historique de ce dossier seront retirées."
                } else {
                    "Seules les fiches à reprendre et en attente seront retirées."
                });
                ui.colored_label(Color32::from_rgb(128,220,198),
                    "Les PNG enregistrés et les images de contrôle restent conservés sur le disque.");
                ui.add_space(8.0);
                ui.horizontal(|ui|{
                    if ui.button("Annuler").clicked(){cancel=true;}
                    if ui.add(egui::Button::new("Confirmer la suppression")
                        .fill(Color32::from_rgb(145,65,61))).clicked(){do_confirm=true;}
                });
            });
        if do_confirm {
            if let Some(id) = self.confirm_delete_visit.take() {
                self.mutate_history(HistoryMutation::Delete(id));
            } else if self.confirm_clear_history.take() == Some(true) {
                self.mutate_history(HistoryMutation::ClearAll);
            } else {
                self.mutate_history(HistoryMutation::ClearMissing);
            }
            self.confirm_clear_history = None;
        } else if cancel || !open {
            self.confirm_delete_visit = None;
            self.confirm_clear_history = None;
        }
    }
}
