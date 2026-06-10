use anyhow::{Context, bail};
use ipnet::IpNet;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(clap::Parser, Debug)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    version = env!("CARGO_PKG_VERSION"),
    bin_name = "ferrumphp",
    about = env!("CARGO_PKG_DESCRIPTION")
)]
pub struct Cli {
    /// Address to bind HTTP server to
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub bind: SocketAddr,

    /// PHP front controller entrypoint
    #[arg(long)]
    pub entrypoint: PathBuf,

    /// Number of PHP workers
    #[arg(long, default_value_t = 10)]
    pub workers: usize,

    /// Trusted proxy CIDRs
    ///
    /// Examples:
    /// --trusted-proxy 127.0.0.1/32
    /// --trusted-proxy 10.0.0.0/8
    /// --trusted-proxy ::1/128
    #[arg(long = "trusted-proxy")]
    pub trusted_proxies: Vec<IpNet>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub entrypoint: PathBuf,
    pub workers: usize,
    pub trusted_proxies: Vec<IpNet>,
}

impl Config {
    pub fn is_trusted_proxy(&self, peer_ip: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .any(|net| net.contains(&peer_ip))
    }
}

impl Cli {
    pub fn validate(self) -> anyhow::Result<Config> {
        // workers validation
        if self.workers == 0 {
            bail!("workers must be greater than zero");
        }

        // entrypoint existence
        if !self.entrypoint.exists() {
            bail!("entrypoint does not exist: {}", self.entrypoint.display());
        }

        // entrypoint must be a file
        if !self.entrypoint.is_file() {
            bail!("entrypoint is not a file: {}", self.entrypoint.display());
        }

        // canonicalize path
        let entrypoint = self
            .entrypoint
            .canonicalize()
            .context("failed to canonicalize entrypoint path")?;

        // optional sanity check
        if entrypoint.extension().and_then(|e| e.to_str()) != Some("php") {
            bail!("entrypoint must be a PHP file: {}", entrypoint.display());
        }

        Ok(Config {
            bind: self.bind,
            entrypoint,
            workers: self.workers,
            trusted_proxies: self.trusted_proxies,
        })
    }
}
