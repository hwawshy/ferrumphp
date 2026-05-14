use crate::php::sapi::Sapi;
use crate::php::worker::Worker;
use bytes::Bytes;
use hyper::http::request::Parts as RequestParts;
use hyper::http::response::Parts as ResponseParts;
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
        let (tx_req, rx_req) = crossbeam_channel::bounded::<Job>(0);
        let handle = std::thread::spawn(move || {
            let _sapi = Sapi::new();
            let mut workers: Vec<Worker> = Vec::with_capacity(10);

            for i in 0..10 {
                workers.push(Worker::new(i, rx_req.clone()))
            }

            while let Some(r) = receiver.blocking_recv() {
                if tx_req.send(r).is_err() {
                    panic!("cannot send request to workers")
                }
            }

            println!("Receiver closed, shutting down workers...");

            // signal workers to shut down
            drop(tx_req);

            for w in workers {
                w.join().unwrap()
            }

            println!("Workers shutdown complete");
        });

        Self { handle }
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.handle.join()
    }
}
