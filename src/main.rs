use crate::php::service::PhpService;
use crate::php::{Job, WorkerPool};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use std::time::Duration;
use clap::Parser;
use tokio::net::TcpListener;
use crate::cli::Cli;

mod php;
mod cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = cli.validate()?;

    let (async_request_sender, async_request_receiver) = tokio::sync::mpsc::channel::<Job>(20);

    let pool = WorkerPool::new(config.workers, async_request_receiver);

    let listener = TcpListener::bind(config.bind).await?;
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
                break;
            }
        }
    }

    tokio::select! {
        result = async {
            graceful.shutdown().await;

            // signal worker pool to shut down
            drop(async_request_sender);

            tokio::task::spawn_blocking(|| pool.join().unwrap()).await
        } => {
            if let Err(e) = result {
                println!("Error while shutting down: {}", e)
            } else {
                println!("Graceful shutdown complete")
            }
        },
        _ = tokio::time::sleep(Duration::from_secs(10)) => {
            println!("Time out while waiting for graceful shutdown, aborting...")
        }
    }

    Ok(())
}
