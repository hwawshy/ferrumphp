use crate::php::sapi::Sapi;
use crate::php::worker::Worker;
use bytes::Bytes;
use hyper::http::request::Parts as RequestParts;
use hyper::http::response::Parts as ResponseParts;
use std::collections::HashMap;
use std::thread::JoinHandle;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Sender as OneshotSender;

mod context;
mod ffi;
mod sapi;
pub mod service;
mod worker;

pub struct Job {
    pub request_head: RequestParts,
    pub request_body_rx: Receiver<Bytes>,
    pub response_head_tx: OneshotSender<ResponseParts>,
    pub response_body_tx: Sender<Bytes>,
}

pub struct WorkerPool {
    handle: JoinHandle<()>,
}

impl WorkerPool {
    pub fn new(mut receiver: Receiver<Job>) -> Self {
        let handle = std::thread::Builder::new()
            .name("SAPI worker".to_string())
            .spawn(move || {
                let _sapi = Sapi::new();

                let (tx_req, rx_req) = crossbeam_channel::bounded::<Job>(0);
                let supervisor = WorkerSupervisor::new(rx_req.clone());

                while let Some(r) = receiver.blocking_recv() {
                    if tx_req.send(r).is_err() {
                        panic!("cannot send request to workers")
                    }
                }

                println!("Receiver closed, shutting down workers...");

                // signal workers to shut down
                drop(tx_req);

                supervisor.join().unwrap();

                println!("Workers shutdown complete");
            })
            .expect("Could not start SAPI worker");

        Self { handle }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}

enum WorkerEvent {
    ErrorExit(usize),
    Exit(usize),
}

struct WorkerSupervisor {
    handle: JoinHandle<()>,
}

impl WorkerSupervisor {
    fn new(job_receiver: crossbeam_channel::Receiver<Job>) -> Self {
        let handle = std::thread::Builder::new()
            .name("Worker supervisor".to_string())
            .spawn(move || {
                let mut workers: HashMap<usize, Worker> = HashMap::with_capacity(10);
                let (event_tx, event_rx) = std::sync::mpsc::sync_channel(10); // @todo rethink this buffer

                for i in 0..10 {
                    workers.insert(
                        i,
                        Worker::new(i as u32, job_receiver.clone(), event_tx.clone()),
                    );
                }

                while let Ok(event) = event_rx.recv() {
                    match event {
                        WorkerEvent::ErrorExit(worker_id) => {
                            println!("restarting worker {worker_id}");

                            if let Some(handle) = workers.remove(&worker_id) {
                                let _ = handle.join();
                            }

                            workers.insert(
                                worker_id,
                                Worker::new(
                                    worker_id as u32,
                                    job_receiver.clone(),
                                    event_tx.clone(),
                                ),
                            );
                        }
                        WorkerEvent::Exit(worker_id) => {
                            if let Some(handle) = workers.remove(&worker_id) {
                                let _ = handle.join();
                            }

                            if workers.is_empty() {
                                break;
                            }
                        }
                    };
                }

                // all workers shutdown gracefully
                for (_, w) in workers {
                    w.join().unwrap()
                }
            })
            .expect("Could not start worker supervisor");

        Self { handle }
    }

    fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}
