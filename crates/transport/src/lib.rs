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
use tokio_socks::tcp::Socks5Stream;

pub const ALPN: &[u8] = b"myremoteapp-control/1";
pub const SERVER_NAME: &str = "remoteapp.local";
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A required proxy route. Failure never falls back to a direct connection.
pub struct Socks5Proxy {
    pub address: String,
    pub credentials: Option<(String, String)>,
}

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
    connect_via(address, config, None).await
}

pub async fn connect_via(
    address: &str,
    config: Arc<ClientConfig>,
    proxy: Option<&Socks5Proxy>,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    timeout(CONNECT_TIMEOUT, async {
        let tcp = if let Some(proxy) = proxy {
            let stream = if let Some((username, password)) = &proxy.credentials {
                ensure!(
                    (1..=255).contains(&username.len()),
                    "SOCKS5 username must be 1-255 bytes"
                );
                ensure!(
                    (1..=255).contains(&password.len()),
                    "SOCKS5 password must be 1-255 bytes"
                );
                Socks5Stream::connect_with_password(
                    proxy.address.as_str(),
                    address,
                    username,
                    password,
                )
                .await
                .context("SOCKS5 authentication or CONNECT failed")?
            } else {
                Socks5Stream::connect(proxy.address.as_str(), address)
                    .await
                    .context("SOCKS5 CONNECT failed")?
            };
            stream.into_inner()
        } else {
            TcpStream::connect(address)
                .await
                .context("direct connection failed")?
        };
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

    // The fake proxy deliberately maps an otherwise unresolvable domain to the
    // loopback host, verifying that the client sends DNS names through SOCKS5.
    async fn proxy_server(
        listener: TcpListener,
        target: std::net::SocketAddr,
        require_password: bool,
        reject: bool,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(socket.read_u8().await.unwrap(), 5);
        let mut methods = vec![0; socket.read_u8().await.unwrap() as usize];
        socket.read_exact(&mut methods).await.unwrap();
        let method = if require_password { 2 } else { 0 };
        assert!(methods.contains(&method));
        socket.write_all(&[5, method]).await.unwrap();
        if require_password {
            assert_eq!(socket.read_u8().await.unwrap(), 1);
            let mut user = vec![0; socket.read_u8().await.unwrap() as usize];
            socket.read_exact(&mut user).await.unwrap();
            let mut password = vec![0; socket.read_u8().await.unwrap() as usize];
            socket.read_exact(&mut password).await.unwrap();
            assert_eq!(user, b"proxy-user");
            let valid = password == b"proxy-password";
            socket.write_all(&[1, u8::from(!valid)]).await.unwrap();
            if !valid {
                return;
            }
        }
        let mut header = [0; 4];
        socket.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..3], &[5, 1, 0]);
        match header[3] {
            3 => {
                let mut domain = vec![0; socket.read_u8().await.unwrap() as usize];
                socket.read_exact(&mut domain).await.unwrap();
                assert_eq!(domain, b"mac.test.invalid");
            }
            1 => {
                let mut ip = [0; 4];
                socket.read_exact(&mut ip).await.unwrap();
            }
            kind => panic!("unexpected address kind {kind}"),
        }
        assert_eq!(socket.read_u16().await.unwrap(), target.port());
        if reject {
            socket
                .write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            return;
        }
        let mut upstream = TcpStream::connect(target).await.unwrap();
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
            .await
            .unwrap();
        // The application may close TLS without close_notify after goodbye.
        let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
    }

    async fn check_proxy(require_password: bool) {
        let (host, host_cert) = identity();
        let (client, client_cert) = identity();
        let server_config = server_config(host, &client_cert).unwrap();
        let client_config = client_config(client, &host_cert).unwrap();
        let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = host.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Socks5Proxy {
            address: listener.local_addr().unwrap().to_string(),
            credentials: require_password.then(|| ("proxy-user".into(), "proxy-password".into())),
        };
        let proxy_task = tokio::spawn(proxy_server(listener, target, require_password, false));
        let host_task = tokio::spawn(async move {
            let (tcp, _) = host.accept().await.unwrap();
            let mut stream = accept(tcp, server_config).await.unwrap();
            remote_session::serve(&mut stream).await.unwrap();
        });
        let mut stream = connect_via(
            &format!("mac.test.invalid:{}", target.port()),
            client_config,
            Some(&proxy),
        )
        .await
        .unwrap();
        remote_session::probe(&mut stream).await.unwrap();
        drop(stream);
        host_task.await.unwrap();
        proxy_task.await.unwrap();
    }

    #[tokio::test]
    async fn socks5_no_auth_with_remote_dns_and_tls() {
        check_proxy(false).await;
    }

    #[tokio::test]
    async fn socks5_password_with_remote_dns_and_tls() {
        check_proxy(true).await;
    }

    #[tokio::test]
    async fn socks5_refusal_does_not_fall_back_to_reachable_host() {
        let (client, _) = identity();
        let (_, host_cert) = identity();
        let client_config = client_config(client, &host_cert).unwrap();
        let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = host.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Socks5Proxy {
            address: listener.local_addr().unwrap().to_string(),
            credentials: None,
        };
        let task = tokio::spawn(proxy_server(listener, target, false, true));
        assert!(
            connect_via(&target.to_string(), client_config, Some(&proxy))
                .await
                .is_err()
        );
        task.await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), host.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn socks5_wrong_password_is_rejected() {
        let (client, _) = identity();
        let (_, host_cert) = identity();
        let client_config = client_config(client, &host_cert).unwrap();
        let host = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = host.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Socks5Proxy {
            address: listener.local_addr().unwrap().to_string(),
            credentials: Some(("proxy-user".into(), "wrong".into())),
        };
        let task = tokio::spawn(proxy_server(listener, target, true, false));
        assert!(
            connect_via(&target.to_string(), client_config, Some(&proxy))
                .await
                .is_err()
        );
        task.await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), host.accept())
                .await
                .is_err()
        );
    }
}
