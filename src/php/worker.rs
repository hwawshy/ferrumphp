use crate::php::Job;
use crate::php::context::WorkerContext;
use crossbeam_channel::Receiver;
use std::thread::JoinHandle;

pub struct Worker {
    handle: JoinHandle<()>,
}

impl Worker {
    pub fn new(id: u32, rx: Receiver<Job>) -> Self {
        let handle = std::thread::spawn(move || {
            let mut ctx = WorkerContext::new(id);

            loop {
                let Ok(job) = rx.recv() else {
                    println!("Worker {} shutting down", id);
                    break;
                };

                ctx.handle_request(
                    job.request_head,
                    job.request_body_rx,
                    job.response_header_tx,
                    job.response_body_tx,
                );
            }
        });

        Self { handle }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}
