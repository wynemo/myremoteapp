use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use remote_transport::{Identity, SERVER_NAME};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{net::TcpListener, sync::Semaphore};

#[derive(Parser)]
#[command(about = "Remote app mutual-TLS control-channel diagnostic")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate a new identity directory. Never overwrites an existing path.
    Init {
        #[arg(long)]
        identity: PathBuf,
    },
    /// Listen for a client whose certificate was explicitly provisioned.
    Serve {
        #[arg(long, default_value = "127.0.0.1:9443")]
        listen: String,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        peer_cert: PathBuf,
    },
    /// Authenticate the host and measure one control-channel round trip.
    Probe {
        #[arg(long)]
        address: String,
        #[arg(long)]
        identity: PathBuf,
        #[arg(long)]
        peer_cert: PathBuf,
    },
}

fn load_identity(directory: &Path) -> Result<Identity> {
    Identity::from_pem(
        &fs::read(directory.join("cert.pem")).context("read identity certificate")?,
        &fs::read(directory.join("key.pem")).context("read identity private key")?,
    )
}

fn init(directory: &Path) -> Result<()> {
    let generated = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder
        .create(directory)
        .context("create a new identity directory (parent must exist)")?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options
        .open(directory.join("key.pem"))?
        .write_all(generated.signing_key.serialize_pem().as_bytes())?;
    options
        .open(directory.join("cert.pem"))?
        .write_all(generated.cert.pem().as_bytes())?;
    println!("Identity created: {}", directory.display());
    println!("Exchange cert.pem with the peer using a trusted channel; keep key.pem private.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Init { identity } => init(&identity)?,
        Command::Probe {
            address,
            identity,
            peer_cert,
        } => {
            let config =
                remote_transport::client_config(load_identity(&identity)?, &fs::read(peer_cert)?)?;
            let mut stream = remote_transport::connect(&address, config).await?;
            let rtt = remote_session::probe(&mut stream).await?;
            println!(
                "Mutual TLS authenticated; protocol v1; control RTT {:.2} ms",
                rtt.as_secs_f64() * 1000.0
            );
        }
        Command::Serve {
            listen,
            identity,
            peer_cert,
        } => {
            let config =
                remote_transport::server_config(load_identity(&identity)?, &fs::read(peer_cert)?)?;
            let listener = TcpListener::bind(&listen).await?;
            let slots = Arc::new(Semaphore::new(8));
            println!(
                "Control diagnostic listening on {} (Ctrl-C to stop)",
                listener.local_addr()?
            );
            loop {
                let (tcp, address) = tokio::select! {
                    result = listener.accept() => result?,
                    result = tokio::signal::ctrl_c() => { result?; break; }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let config = config.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let mut stream = remote_transport::accept(tcp, config).await?;
                        remote_session::serve(&mut stream).await
                    }
                    .await;
                    match result {
                        Ok(()) => println!("{address}: diagnostic completed"),
                        Err(error) => eprintln!("{address}: {error:#}"),
                    }
                });
            }
        }
    }
    Ok(())
}
