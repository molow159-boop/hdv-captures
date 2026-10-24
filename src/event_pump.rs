//! Keeps persistence and the manual shortcut alive even when rendering is hidden.
use super::*;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc,
};
pub struct Pump {
    pub blocked: Arc<AtomicBool>,
    pub hotkey: Arc<AtomicU32>,
    pub finished: Arc<AtomicBool>,
}
pub fn spawn(
    rx: Receiver<WorkerEvent>,
    ui: Sender<WorkerEvent>,
    visits: Sender<VisitWrite>,
    commands: Sender<WorkerCommand>,
    ctx: egui::Context,
    hotkey: u32,
) -> Pump {
    let blocked = Arc::new(AtomicBool::new(true));
    let key = Arc::new(AtomicU32::new(hotkey));
    let finished = Arc::new(AtomicBool::new(false));
    let pump = Pump {
        blocked: blocked.clone(),
        hotkey: key.clone(),
        finished: finished.clone(),
    };
    std::thread::spawn(move || {
        let mut last_key = Instant::now() - Duration::from_secs(5);
        loop {
            while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                if event.id == key.load(Ordering::SeqCst)
                    && event.state == HotKeyState::Pressed
                    && !blocked.load(Ordering::SeqCst)
                    && last_key.elapsed() > Duration::from_millis(600)
                {
                    last_key = Instant::now();
                    let _ = commands.send(WorkerCommand::Manual);
                }
            }
            match rx.recv_timeout(Duration::from_millis(25)) {
                Ok(event) => {
                    if let WorkerEvent::Scoped {
                        folder,
                        session,
                        event,
                    } = &event
                    {
                        match event.as_ref() {
                            WorkerEvent::Visit {
                                id,
                                name,
                                state,
                                reason,
                                evidence,
                            } => {
                                let observed = chrono::DateTime::from_timestamp_micros(*id)
                                    .map(|time| {
                                        time.with_timezone(&chrono::Local)
                                            .format("%Y-%m-%d %H:%M:%S")
                                            .to_string()
                                    })
                                    .unwrap_or_else(|| chrono::Local::now().to_rfc3339());
                                let record = index::VisitRecord {
                                    id: *id,
                                    name: name.clone().unwrap_or_default(),
                                    state: state.clone(),
                                    reason: reason.clone(),
                                    observed_at: observed,
                                    evidence: evidence
                                        .as_ref()
                                        .map(|p| p.to_string_lossy().into_owned())
                                        .unwrap_or_default(),
                                    session_id: session.clone(),
                                };
                                let _ = visits.send(VisitWrite::Store(folder.clone(), record));
                            }
                            WorkerEvent::Evidence { id, path } => {
                                let _ = visits.send(VisitWrite::Evidence(
                                    folder.clone(),
                                    *id,
                                    path.clone(),
                                ));
                            }
                            _ => {}
                        }
                    }
                    if ui.send(event).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        finished.store(true, Ordering::SeqCst);
    });
    pump
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn background_events_keep_their_origin_without_a_render_loop() {
        let base = std::env::temp_dir().join(format!("hdv-pump-{}", std::process::id()));
        let original = base.join("origine");
        let other = base.join("autre");
        let (tx, rx) = mpsc::channel();
        let (ui_tx, ui_rx) = mpsc::channel();
        let (visits_tx, visits_rx) = mpsc::channel();
        let (commands, _) = mpsc::channel();
        let writer = std::thread::spawn(move || run_visit_writer(visits_rx));
        let pump = spawn(
            rx,
            ui_tx,
            visits_tx.clone(),
            commands,
            egui::Context::default(),
            0,
        );
        let id = chrono::Local::now().timestamp_micros();
        tx.send(WorkerEvent::Scoped {
            folder: original.clone(),
            session: "origine".into(),
            event: Box::new(WorkerEvent::Visit {
                id,
                name: Some("Objet Test".into()),
                state: VisitState::Saved,
                reason: "PNG enregistré".into(),
                evidence: None,
            }),
        })
        .unwrap();
        drop(tx);
        ui_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let start = Instant::now();
        while !pump.finished.load(Ordering::SeqCst) && start.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(pump.finished.load(Ordering::SeqCst));
        let (ack, reply) = mpsc::channel();
        visits_tx.send(VisitWrite::Flush(ack)).unwrap();
        reply.recv_timeout(Duration::from_secs(3)).unwrap();
        let records = index::recent_visits(&original, 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].session_id, "origine");
        assert!(index::recent_visits(&other, 10).unwrap().is_empty());
        drop(visits_tx);
        writer.join().unwrap();
    }
}
