use crate::php::service::PhpService;
use crate::php::{Job, WorkerPool};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;

mod php;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (async_request_sender, async_request_receiver) = tokio::sync::mpsc::channel::<Job>(20);

    let pool = WorkerPool::new(async_request_receiver);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = TcpListener::bind(addr).await?;
    let builder = auto::Builder::new(TokioExecutor::new());
    let graceful = GracefulShutdown::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result?;
                let tx = async_request_sender.clone();
                let service = TowerToHyperService::new(PhpService::new(tx));
                let conn = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
                let fut = graceful.watch(conn.into_owned());

                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        eprintln!("server error: {}", e);
                    }
                });
            },
            _ = tokio::signal::ctrl_c() => {
                println!("Starting graceful shutdown");
                // signal worker pool to shut down
                drop((async_request_sender));
                break;
            }
        }
    }

    match timeout(Duration::from_secs(10), async {
        graceful.shutdown().await;

        tokio::task::spawn_blocking(|| pool.join().unwrap()).await
    })
    .await
    {
        Err(_) => println!("Time out while waiting for graceful shutdown, aborting..."),
        Ok(Err(e)) => println!("Error while shutting down: {}", e),
        Ok(Ok(())) => println!("Graceful shutdown complete"),
    }

    Ok(())
}
