use crate::php::service::PhpService;
use crate::php::{Job, WorkerPool};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use std::net::SocketAddr;
use tokio::net::TcpListener;

mod php;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (async_request_sender, async_request_receiver) = tokio::sync::mpsc::channel::<Job>(20);

    let _pool = WorkerPool::new(async_request_receiver);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        let tx = async_request_sender.clone();
        tokio::spawn(async move {
            let service = TowerToHyperService::new(PhpService::new(tx));
            if let Err(e) = auto::Builder::new(TokioExecutor::new())
                .serve_connection(io, service)
                .await
            {
                eprintln!("server error: {}", e);
            }
        });
    }
}
