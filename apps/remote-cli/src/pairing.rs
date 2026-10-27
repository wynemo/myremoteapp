//! Keep certificate provisioning out of the user flow. The exported connection
//! file is a reusable access credential, not a public certificate or one-time code.
use anyhow::{Context, Result, ensure};
use remote_transport::{Identity, SERVER_NAME};
use serde::{Deserialize, Serialize};
use std::{fs, io::Write, path::Path};

const FILE_VERSION: u16 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionFile {
    version: u16,
    pub host_certificate: String,
    pub client_certificate: String,
    pub client_key: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostState {
    pub host_key: String,
    pub connection: ConnectionFile,
}

impl ConnectionFile {
    pub fn load(path: &Path) -> Result<Self> {
        let connection: Self = serde_json::from_slice(&read_bounded(path)?)
            .context("invalid connection file; copy connect.json from the Mac")?;
        connection.validate()?;
        Ok(connection)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == FILE_VERSION,
            "unsupported connection file version"
        );
        Identity::from_pem(
            self.client_certificate.as_bytes(),
            self.client_key.as_bytes(),
        )?;
        Ok(())
    }
}

pub fn load_or_create(directory: &Path, reset: bool) -> Result<HostState> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    let mut builder = builder;
    builder.recursive(true).create(directory)?;
    let state_path = directory.join("identity.json");
    let state: HostState = if state_path.try_exists()? && !reset {
        serde_json::from_slice(&read_bounded(&state_path)?)
            .context("cannot load saved identity; use --reset-pairing to replace it")?
    } else {
        let host = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
        let client = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_owned()])?;
        let state = HostState {
            host_key: host.signing_key.serialize_pem(),
            connection: ConnectionFile {
                version: FILE_VERSION,
                host_certificate: host.cert.pem(),
                client_certificate: client.cert.pem(),
                client_key: client.signing_key.serialize_pem(),
            },
        };
        save_private(&state_path, &serde_json::to_vec_pretty(&state)?, reset)?;
        state
    };
    state.connection.validate()?;
    Identity::from_pem(
        state.connection.host_certificate.as_bytes(),
        state.host_key.as_bytes(),
    )?;
    save_private(
        &directory.join("connect.json"),
        &serde_json::to_vec_pretty(&state.connection)?,
        true,
    )?;
    Ok(state)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 65536,
        "connection/identity file is too large"
    );
    Ok(bytes)
}

fn save_private(path: &Path, contents: &[u8], overwrite: bool) -> Result<()> {
    // tempfile creates mode-0600 files on Unix. Persist only complete files.
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    if overwrite {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_preserves_credentials_and_reset_replaces_them() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("host");
        let original = load_or_create(&directory, false).unwrap();
        let exported = ConnectionFile::load(&directory.join("connect.json")).unwrap();
        assert_eq!(
            exported.host_certificate,
            original.connection.host_certificate
        );
        assert!(
            !fs::read_to_string(directory.join("connect.json"))
                .unwrap()
                .contains(&original.host_key)
        );
        let restarted = load_or_create(&directory, false).unwrap();
        assert_eq!(original.host_key, restarted.host_key);
        assert_eq!(
            original.connection.client_key,
            restarted.connection.client_key
        );
        let reset = load_or_create(&directory, true).unwrap();
        assert_ne!(original.host_key, reset.host_key);
        assert_ne!(original.connection.client_key, reset.connection.client_key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["identity.json", "connect.json"] {
                assert_eq!(
                    fs::metadata(directory.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn corrupt_saved_identity_is_not_silently_replaced() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("identity.json"), b"bad json").unwrap();
        assert!(load_or_create(directory.path(), false).is_err());
        assert_eq!(
            fs::read(directory.path().join("identity.json")).unwrap(),
            b"bad json"
        );
    }
}
