use crate::php::context::WorkerContext;
use crate::php::{Job, WorkerEvent};
use crossbeam_channel::Receiver;
use std::thread::JoinHandle;

pub struct Worker {
    handle: JoinHandle<()>,
}

impl Worker {
    pub fn new(
        id: usize,
        rx: Receiver<Job>,
        event_tx: std::sync::mpsc::SyncSender<WorkerEvent>,
    ) -> Self {
        let handle = std::thread::spawn(move || {
            let mut ctx = WorkerContext::new(id);

            while let Ok(job) = rx.recv() {
                if let Err(_) = ctx.handle_request(
                    job.request_head,
                    job.request_body_rx,
                    job.response_head_tx,
                    job.response_body_tx,
                ) {
                    let _ = event_tx.send(WorkerEvent::ErrorExit(id));
                    return;
                }
            }

            println!("Worker {} shutting down", id);
            let _ = event_tx.send(WorkerEvent::Exit(id));
        });

        Self { handle }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}
