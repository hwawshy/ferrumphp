use crate::php::sapi::Sapi;
use crate::php::worker::Worker;
use hyper::body::Incoming;
use hyper::{Request, Response};
use std::thread::JoinHandle;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;

mod sapi;
pub mod service;
mod worker;

pub struct Job {
    pub request: Request<Incoming>,
    pub respond_to: Sender<Response<String>>,
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

            for w in workers {
                w.join().unwrap()
            }
        });

        Self { handle }
    }
}
