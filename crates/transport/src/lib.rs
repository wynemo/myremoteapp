//! Mutual TLS with an explicitly provisioned peer certificate, never OS trust roots.
use anyhow::{Context, Result, ensure};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName},
    server::WebPkiClientVerifier,
};
use std::{io::BufReader, sync::Arc, time::Duration};
use tokio::{net::TcpStream, time::timeout};
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub const ALPN: &[u8] = b"myremoteapp-control/1";
pub const SERVER_NAME: &str = "remoteapp.local";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Identity {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl Identity {
    pub fn from_pem(cert: &[u8], key: &[u8]) -> Result<Self> {
        let certs = certificates(cert)?;
        let key = rustls_pemfile::private_key(&mut BufReader::new(key))?
            .context("no private key found")?;
        Ok(Self { certs, key })
    }
}

fn certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let certs =
        rustls_pemfile::certs(&mut BufReader::new(pem)).collect::<std::io::Result<Vec<_>>>()?;
    ensure!(!certs.is_empty(), "no certificates found");
    Ok(certs)
}

fn peer_roots(peer: &[u8]) -> Result<RootCertStore> {
    let certs = certificates(peer)?;
    ensure!(
        certs.len() == 1,
        "expected exactly one trusted peer certificate"
    );
    let mut roots = RootCertStore::empty();
    roots.add(certs.into_iter().next().unwrap())?;
    Ok(roots)
}

pub fn server_config(identity: Identity, peer: &[u8]) -> Result<Arc<ServerConfig>> {
    let verifier = WebPkiClientVerifier::builder_with_provider(
        Arc::new(peer_roots(peer)?),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()?;
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(verifier)
            .with_single_cert(identity.certs, identity.key)?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub fn client_config(identity: Identity, peer: &[u8]) -> Result<Arc<ClientConfig>> {
    let mut config =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_root_certificates(peer_roots(peer)?)
            .with_client_auth_cert(identity.certs, identity.key)?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub async fn connect(
    address: &str,
    config: Arc<ClientConfig>,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    timeout(CONNECT_TIMEOUT, async {
        let tcp = TcpStream::connect(address).await?;
        tcp.set_nodelay(true)?;
        let stream = TlsConnector::from(config)
            .connect(ServerName::try_from(SERVER_NAME)?, tcp)
            .await?;
        ensure!(
            stream.get_ref().1.alpn_protocol() == Some(ALPN),
            "control ALPN mismatch"
        );
        Ok(stream)
    })
    .await
    .context("connection timed out")?
}

pub async fn accept(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<tokio_rustls::server::TlsStream<TcpStream>> {
    tcp.set_nodelay(true)?;
    let stream = timeout(CONNECT_TIMEOUT, TlsAcceptor::from(config).accept(tcp))
        .await
        .context("TLS handshake timed out")??;
    ensure!(
        stream.get_ref().1.alpn_protocol() == Some(ALPN),
        "control ALPN mismatch"
    );
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    fn identity() -> (Identity, Vec<u8>) {
        let generated = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()]).unwrap();
        let cert = generated.cert.pem().into_bytes();
        let identity =
            Identity::from_pem(&cert, generated.signing_key.serialize_pem().as_bytes()).unwrap();
        (identity, cert)
    }

    #[tokio::test]
    async fn trusted_peers_complete_tls_and_session() {
        let (host, host_cert) = identity();
        let (client, client_cert) = identity();
        let server = server_config(host, &client_cert).unwrap();
        let client = client_config(client, &host_cert).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let host = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut stream = accept(tcp, server).await.unwrap();
            remote_session::serve(&mut stream).await.unwrap();
        });
        let mut stream = connect(&address, client).await.unwrap();
        remote_session::probe(&mut stream).await.unwrap();
        host.await.unwrap();
    }

    #[tokio::test]
    async fn unpaired_client_is_rejected() {
        let (host, host_cert) = identity();
        let (_, trusted_cert) = identity();
        let (stranger, _) = identity();
        let server = server_config(host, &trusted_cert).unwrap();
        let client = client_config(stranger, &host_cert).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let host = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            assert!(accept(tcp, server).await.is_err());
        });
        // TLS 1.3 client may finish locally before receiving the server's rejection.
        if let Ok(mut stream) = connect(&address, client).await {
            assert!(remote_session::probe(&mut stream).await.is_err());
        }
        host.await.unwrap();
    }

    #[tokio::test]
    async fn untrusted_server_is_rejected() {
        let (host, _) = identity();
        let (client, client_cert) = identity();
        let (_, wrong_host_cert) = identity();
        let server = server_config(host, &client_cert).unwrap();
        let client = client_config(client, &wrong_host_cert).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let host = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            assert!(accept(tcp, server).await.is_err());
        });
        assert!(connect(&address, client).await.is_err());
        host.await.unwrap();
    }
}
