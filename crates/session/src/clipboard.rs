//! Clipboard sockets are capabilities scoped to an exclusive input lease.
//! Native operations run off the async runtime; content is never logged.
use crate::IO_TIMEOUT;
use anyhow::{Result, bail, ensure};
use remote_protocol::{ClipboardFormat, Message, read_message, write_message};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{mpsc, watch},
    time::{Duration, timeout},
};

pub const MAX_TEXT: usize = 1024 * 1024;
pub const MAX_IMAGE: usize = 32 * 1024 * 1024;
pub const MAX_PIXELS: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct Content {
    pub format: ClipboardFormat,
    pub bytes: Arc<Vec<u8>>,
}
impl Content {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.bytes.is_empty(), "empty clipboard content");
        match self.format {
            ClipboardFormat::Text => {
                ensure!(self.bytes.len() <= MAX_TEXT, "clipboard text exceeds limit");
                let text = std::str::from_utf8(&self.bytes)?;
                ensure!(!text.contains('\0'), "clipboard text contains NUL");
            }
            ClipboardFormat::Png => ensure!(
                self.bytes.len() <= MAX_IMAGE,
                "clipboard image exceeds limit"
            ),
        }
        Ok(())
    }
}

/// Implementations check desktop availability and the native version again
/// immediately before writing. A mismatch leaves the user's clipboard intact.
pub trait Backend: Send + Sync {
    fn available(&self) -> bool;
    fn version(&self) -> u64;
    fn read(&self) -> Result<Option<Content>>;
    fn write(&self, expected: u64, content: &Content) -> Result<Option<u64>>;
    /// Native backends recheck this after expensive decoding, immediately before
    /// changing the system clipboard, so cancelled work cannot commit later.
    fn write_guarded(
        &self,
        expected: u64,
        content: &Content,
        allowed: &(dyn Fn() -> bool + Sync),
    ) -> Result<Option<u64>> {
        if allowed() {
            self.write(expected, content)
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Copy)]
pub struct Permissions {
    pub send: bool,
    pub receive: bool,
}
impl Permissions {
    pub fn enabled(self) -> bool {
        self.send || self.receive
    }
}

struct Capability {
    token: String,
    active: watch::Sender<bool>,
    attached: bool,
}
pub struct Host {
    pub backend: Arc<dyn Backend>,
    pub permissions: Permissions,
    capability: Mutex<Option<Capability>>,
}
impl Host {
    pub fn new(backend: Arc<dyn Backend>, permissions: Permissions) -> Arc<Self> {
        Arc::new(Self {
            backend,
            permissions,
            capability: Mutex::new(None),
        })
    }
    pub(crate) fn lease(self: &Arc<Self>) -> Result<Lease> {
        let mut random = [0u8; 32];
        getrandom::fill(&mut random)
            .map_err(|_| anyhow::anyhow!("clipboard capability unavailable"))?;
        let token = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let (active, _) = watch::channel(true);
        let mut current = self.capability.lock().unwrap();
        if let Some(previous) = current.take() {
            previous.active.send_replace(false);
        }
        *current = Some(Capability {
            token: token.clone(),
            active,
            attached: false,
        });
        Ok(Lease {
            host: self.clone(),
            token,
        })
    }
    pub(crate) fn attach(&self, token: &str) -> Result<watch::Receiver<bool>> {
        let mut guard = self.capability.lock().unwrap();
        let cap = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("clipboard control lease expired"))?;
        ensure!(
            token.len() == 64 && cap.token == token && !cap.attached,
            "invalid clipboard capability"
        );
        cap.attached = true;
        Ok(cap.active.subscribe())
    }
}
pub(crate) struct Lease {
    host: Arc<Host>,
    pub token: String,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut cap = self.host.capability.lock().unwrap();
        if cap.as_ref().is_some_and(|c| c.token == self.token)
            && let Some(cap) = cap.take()
        {
            cap.active.send_replace(false);
        }
    }
}

struct Packet {
    message: Message,
    content: Option<Content>,
}
// A dedicated reader keeps partial reads alive across timer ticks. Cancelling a
// read_message future in select! would corrupt framing on a slow proxy.
async fn read_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
    offer: &mut Option<(u64, ClipboardFormat, usize)>,
) -> Result<Packet> {
    let message = timeout(IO_TIMEOUT, read_message(reader)).await??;
    let mut content = None;
    match &message {
        Message::ClipboardOffer {
            revision,
            format,
            length,
            ..
        } => {
            let limit = match format {
                ClipboardFormat::Text => MAX_TEXT,
                ClipboardFormat::Png => MAX_IMAGE,
            };
            ensure!(
                *length > 0 && *length as usize <= limit,
                "clipboard offer exceeds limit"
            );
            *offer = Some((*revision, *format, *length as usize));
        }
        Message::ClipboardData { revision } => {
            let (expected, format, length) = offer
                .take()
                .ok_or_else(|| anyhow::anyhow!("unsolicited clipboard data"))?;
            ensure!(*revision == expected, "clipboard revision mismatch");
            let mut bytes = vec![0; length];
            timeout(IO_TIMEOUT, reader.read_exact(&mut bytes)).await??;
            let value = Content {
                format,
                bytes: Arc::new(bytes),
            };
            value.validate()?;
            content = Some(value);
        }
        _ => {}
    }
    Ok(Packet { message, content })
}
async fn send<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
    content: Option<&Content>,
) -> Result<()> {
    timeout(IO_TIMEOUT, async {
        write_message(writer, message).await?;
        if let Some(content) = content {
            // Keep writes bounded rather than passing a huge slice downstream.
            for chunk in content.bytes.chunks(64 * 1024) {
                writer.write_all(chunk).await?;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await??;
    Ok(())
}
async fn advertise<W: AsyncWrite + Unpin>(
    writer: &mut W,
    revision: u64,
    generation: u64,
    content: &Content,
) -> Result<()> {
    send(
        writer,
        &Message::ClipboardOffer {
            revision,
            generation,
            format: content.format,
            length: content.bytes.len() as u32,
        },
        None,
    )
    .await
}

pub async fn start<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    token: String,
    permissions: Permissions,
) -> Result<Permissions> {
    send(
        stream,
        &Message::Hello {
            version: remote_protocol::VERSION,
        },
        None,
    )
    .await?;
    ensure!(
        timeout(IO_TIMEOUT, read_message(stream)).await??
            == (Message::Welcome {
                version: remote_protocol::VERSION
            }),
        "clipboard protocol mismatch"
    );
    send(
        stream,
        &Message::StartClipboard {
            token,
            send: permissions.send,
            receive: permissions.receive,
        },
        None,
    )
    .await?;
    match timeout(IO_TIMEOUT, read_message(stream)).await?? {
        Message::ClipboardReady { send, receive } => Ok(Permissions { send, receive }),
        _ => bail!("clipboard unavailable"),
    }
}

/// Server generations arbitrate simultaneous copies. Client proposals carry
/// their last observed generation; stale proposals are retried after rejection.
#[derive(Default)]
pub struct Progress {
    pub settled_version: AtomicU64,
    pub healthy: AtomicBool,
}
struct ProgressGuard(Option<Arc<Progress>>);
impl Drop for ProgressGuard {
    fn drop(&mut self) {
        if let Some(progress) = &self.0 {
            progress.healthy.store(false, Ordering::Release);
        }
    }
}

pub async fn exchange<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    backend: Arc<dyn Backend>,
    permissions: Permissions,
    server: bool,
) -> Result<()> {
    exchange_with_progress(stream, backend, permissions, server, None).await
}
pub async fn exchange_with_progress<S: AsyncRead + AsyncWrite + Unpin + Send>(
    stream: S,
    backend: Arc<dyn Backend>,
    permissions: Permissions,
    server: bool,
    progress: Option<Arc<Progress>>,
) -> Result<()> {
    let _progress_guard = ProgressGuard(progress.clone());
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (packets, mut incoming) = mpsc::channel(1);
    let reading = async {
        let mut offer = None;
        loop {
            let packet = read_packet(&mut reader, &mut offer).await?;
            if packets.send(packet).await.is_err() {
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let processing = async {
        let mut tick = tokio::time::interval(Duration::from_millis(150));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut heartbeat = tokio::time::interval(Duration::from_secs(2));
        let mut version = backend.version();
        let settle = |version| {
            if let Some(progress) = &progress {
                progress.settled_version.store(version, Ordering::Release);
            }
        };
        settle(version);
        if let Some(progress) = &progress {
            progress.healthy.store(true, Ordering::Release);
        }
        let mut pending_version = version;
        let mut available = backend.available();
        let mut generation = 0u64;
        let mut revision = 0u64;
        let mut pending: Option<(u64, Content)> = None;
        // Incoming revision, server generation, native clipboard version.
        let mut receiving: Option<(u64, u64, u64)> = None;
        let mut retry = false;
        loop {
            tokio::select! {
                _ = heartbeat.tick() => send(&mut writer, &Message::Ping {sequence:0}, None).await?,
                _ = tick.tick() => {
                    let now_available = backend.available();
                    if !now_available || !available {
                        version = backend.version();
                        pending = None;
                        receiving = None;
                        if available && !now_available { send(&mut writer, &Message::ClipboardPaused, None).await?; }
                        available = now_available;
                        continue;
                    }
                    let current = backend.version();
                    if current == version {
                        if retry && let Some((id,content))=&pending {
                            advertise(&mut writer,*id,generation,content).await?;
                        }
                        retry=false;
                        continue;
                    }
                    version = current;
                    pending = None;
                    if !permissions.send { continue; }
                    let native = backend.clone();
                    let value = tokio::task::spawn_blocking(move || native.read()).await??;
                    // Never advertise an inconsistent snapshot.
                    if current != backend.version() { continue; }
                    if let Some(content) = value {
                        content.validate()?;
                        revision = revision.checked_add(1).ok_or_else(|| anyhow::anyhow!("clipboard revision exhausted"))?;
                        if server { generation += 1; }
                        advertise(&mut writer, revision, generation, &content).await?;
                        pending_version=current;
                        pending = Some((revision,content));
                    }
                }
                packet = incoming.recv() => {
                    let Some(packet) = packet else { bail!("clipboard disconnected"); };
                    match packet.message {
                        Message::Ping {sequence} => send(&mut writer, &Message::Pong {sequence}, None).await?,
                        Message::Pong {..} => {},
                        Message::ClipboardPaused => { receiving=None; pending=None; version=backend.version(); },
                        Message::ClipboardOffer { revision: id, generation: offered, .. } => {
                            ensure!(permissions.receive, "clipboard receiving disabled");
                            if !backend.available() || (server && offered != generation) || (!server && offered < generation) {
                                send(&mut writer,&Message::ClipboardRejected {revision:id,generation},None).await?;
                                continue;
                            }
                            if !server { generation = offered; }
                            receiving = Some((id,offered,backend.version()));
                            send(&mut writer,&Message::ClipboardRequest {revision:id},None).await?;
                        }
                        Message::ClipboardRequest {revision:id} => {
                            ensure!(permissions.send, "clipboard sending disabled");
                            if backend.available() && let Some((sent,content))=&pending && *sent==id {
                                send(&mut writer,&Message::ClipboardData {revision:id},Some(content)).await?;
                            }
                        }
                        Message::ClipboardData {revision:id} => {
                            let Some((expected,offered,baseline)) = receiving.take() else { continue; };
                            if id!=expected || !backend.available() { continue; }
                            // Detect a local copy even when it occurred between timer ticks.
                            if server && (offered!=generation || baseline!=version) {
                                send(&mut writer,&Message::ClipboardRejected {revision:id,generation},None).await?;
                                continue;
                            }
                            let content=packet.content.ok_or_else(|| anyhow::anyhow!("missing clipboard content"))?;
                            let native=backend.clone();
                            let to_write=content.clone();
                            let result=tokio::task::spawn_blocking(move || native.write(baseline,&to_write)).await??;
                            if let Some(written)=result {
                                version=written;
                                settle(written);
                                pending=None;
                                if server { generation+=1; }
                                send(&mut writer,&Message::ClipboardApplied {revision:id,generation},None).await?;
                                // Echo the authoritative commit. Otherwise crossing
                                // copies could leave each endpoint with the other's
                                // old value despite both transfers being acknowledged.
                                if server && permissions.send {
                                    revision+=1;
                                    advertise(&mut writer,revision,generation,&content).await?;
                                    pending=Some((revision,content));
                                }
                            } else {
                                send(&mut writer,&Message::ClipboardRejected {revision:id,generation},None).await?;
                            }
                        }
                        Message::ClipboardApplied {revision:id,generation:applied} => {
                            if !server { generation=generation.max(applied); }
                            if pending.as_ref().is_some_and(|(sent,_)| *sent==id) { settle(pending_version); pending=None; }
                            else if !server && let Some((id,content))=&pending {
                                advertise(&mut writer,*id,generation,content).await?;
                            }
                        }
                        Message::ClipboardRejected {revision:id,generation:current} => {
                            if !server {
                                generation=generation.max(current);
                                if backend.available() && let Some((sent,content))=&pending && *sent==id {
                                    let _=content;
                                    retry=true;
                                }
                            }
                        }
                        Message::Goodbye => return Ok(()),
                        _ => bail!("unexpected clipboard message"),
                    }
                }
            }
        }
    };
    tokio::pin!(reading, processing);
    tokio::select! { result=&mut reading=>result, result=&mut processing=>result }
}

struct LeasedBackend {
    inner: Arc<dyn Backend>,
    active: watch::Receiver<bool>,
}
impl Backend for LeasedBackend {
    fn available(&self) -> bool {
        *self.active.borrow() && self.inner.available()
    }
    fn version(&self) -> u64 {
        self.inner.version()
    }
    fn read(&self) -> Result<Option<Content>> {
        if !self.available() {
            return Ok(None);
        }
        self.inner.read()
    }
    fn write(&self, expected: u64, content: &Content) -> Result<Option<u64>> {
        if !self.available() {
            return Ok(None);
        }
        self.inner
            .write_guarded(expected, content, &|| self.available())
    }
}
pub(crate) fn leased_backend(
    inner: Arc<dyn Backend>,
    active: watch::Receiver<bool>,
) -> Arc<dyn Backend> {
    Arc::new(LeasedBackend { inner, active })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    struct Memory {
        value: Mutex<(u64, Option<Content>)>,
        available: AtomicBool,
        writes: AtomicUsize,
    }
    impl Memory {
        fn new(text: &str) -> Arc<Self> {
            Arc::new(Self {
                value: Mutex::new((1, Some(text_content(text)))),
                available: AtomicBool::new(true),
                writes: AtomicUsize::new(0),
            })
        }
        fn copy(&self, content: Content) {
            let mut guard = self.value.lock().unwrap();
            guard.0 += 1;
            guard.1 = Some(content);
        }
        fn bytes(&self) -> Vec<u8> {
            self.value
                .lock()
                .unwrap()
                .1
                .as_ref()
                .unwrap()
                .bytes
                .as_ref()
                .clone()
        }
    }
    impl Backend for Memory {
        fn available(&self) -> bool {
            self.available.load(Ordering::Acquire)
        }
        fn version(&self) -> u64 {
            self.value.lock().unwrap().0
        }
        fn read(&self) -> Result<Option<Content>> {
            Ok(self.value.lock().unwrap().1.clone())
        }
        fn write(&self, expected: u64, content: &Content) -> Result<Option<u64>> {
            let mut guard = self.value.lock().unwrap();
            if guard.0 != expected || !self.available() {
                return Ok(None);
            }
            guard.0 += 1;
            guard.1 = Some(content.clone());
            self.writes.fetch_add(1, Ordering::Relaxed);
            Ok(Some(guard.0))
        }
    }
    fn text_content(text: &str) -> Content {
        Content {
            format: ClipboardFormat::Text,
            bytes: Arc::new(text.as_bytes().to_vec()),
        }
    }
    const BOTH: Permissions = Permissions {
        send: true,
        receive: true,
    };
    async fn until(mut condition: impl FnMut() -> bool) {
        timeout(Duration::from_secs(4), async {
            while !condition() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    fn pair(
        a: Arc<Memory>,
        b: Arc<Memory>,
    ) -> (
        tokio::task::JoinHandle<Result<()>>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (client, server) = tokio::io::duplex(2048);
        (
            tokio::spawn(exchange(server, a, BOTH, true)),
            tokio::spawn(exchange(client, b, BOTH, false)),
        )
    }
    #[test]
    fn rejects_invalid_text_and_bounds() {
        assert!(text_content("中文🙂\n").validate().is_ok());
        assert!(text_content("x\0y").validate().is_err());
        assert!(text_content("").validate().is_err());
        assert!(
            Content {
                format: ClipboardFormat::Text,
                bytes: Arc::new(vec![255])
            }
            .validate()
            .is_err()
        );
        assert!(
            Content {
                format: ClipboardFormat::Text,
                bytes: Arc::new(vec![b'x'; MAX_TEXT + 1])
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn capability_is_single_use_and_expires_with_control() {
        let host = Host::new(Memory::new("host"), BOTH);
        assert!(host.attach(&"a".repeat(64)).is_err());
        let lease = host.lease().unwrap();
        assert!(host.attach(&"b".repeat(64)).is_err());
        let active = host.attach(&lease.token).unwrap();
        assert!(host.attach(&lease.token).is_err());
        let backend = leased_backend(host.backend.clone(), active.clone());
        drop(lease);
        assert!(!*active.borrow());
        assert!(!backend.available());
        assert!(
            backend
                .write(1, &text_content("forbidden"))
                .unwrap()
                .is_none()
        );
    }
    #[tokio::test]
    async fn rejects_large_offer_before_allocating_and_unsolicited_data() {
        for message in [
            Message::ClipboardOffer {
                revision: 1,
                generation: 0,
                format: ClipboardFormat::Text,
                length: MAX_TEXT as u32 + 1,
            },
            Message::ClipboardData { revision: 1 },
        ] {
            let (mut client, mut server) = tokio::io::duplex(1024);
            write_message(&mut client, &message).await.unwrap();
            assert!(read_packet(&mut server, &mut None).await.is_err());
        }
    }
    #[tokio::test]
    async fn bidirectional_text_large_image_no_initial_overwrite_or_echo_loop() {
        let a = Memory::new("host existing");
        let b = Memory::new("client existing");
        let (server, client) = pair(a.clone(), b.clone());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(a.bytes(), b"host existing");
        assert_eq!(b.bytes(), b"client existing");
        b.copy(text_content("中文🙂\nsecond line"));
        until(|| a.bytes() == "中文🙂\nsecond line".as_bytes()).await;
        let image = Content {
            format: ClipboardFormat::Png,
            bytes: Arc::new(vec![42; 180_000]),
        };
        a.copy(image.clone());
        until(|| b.bytes() == *image.bytes).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let versions = (a.version(), b.version());
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(versions, (a.version(), b.version()));
        assert!(!server.is_finished() && !client.is_finished());
        server.abort();
        client.abort();
    }
    #[tokio::test]
    async fn simultaneous_copies_converge() {
        let a = Memory::new("a");
        let b = Memory::new("b");
        let (server, client) = pair(a.clone(), b.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        for i in 0..8 {
            a.copy(text_content(&format!("host {i}")));
            b.copy(text_content(&format!("client {i}")));
            until(|| a.bytes() == b.bytes()).await;
            tokio::time::sleep(Duration::from_millis(170)).await;
            assert_eq!(a.bytes(), b.bytes());
        }
        assert!(!server.is_finished() && !client.is_finished());
        server.abort();
        client.abort();
    }
    #[tokio::test]
    async fn local_copy_during_transfer_is_not_overwritten() {
        let native = Memory::new("original");
        let (mut remote, stream) = tokio::io::duplex(1024);
        let task = tokio::spawn(exchange(
            stream,
            native.clone(),
            Permissions {
                send: false,
                receive: true,
            },
            false,
        ));
        advertise(&mut remote, 1, 1, &text_content("old remote"))
            .await
            .unwrap();
        loop {
            if matches!(
                read_message(&mut remote).await.unwrap(),
                Message::ClipboardRequest { .. }
            ) {
                break;
            }
        }
        native.copy(text_content("new local"));
        send(
            &mut remote,
            &Message::ClipboardData { revision: 1 },
            Some(&text_content("old remote")),
        )
        .await
        .unwrap();
        until(|| native.version() == 2).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(native.bytes(), b"new local");
        assert_eq!(native.writes.load(Ordering::Relaxed), 0);
        task.abort();
    }
    #[tokio::test]
    async fn paused_changes_are_not_replayed_on_resume() {
        let a = Memory::new("a");
        let b = Memory::new("b");
        let (server, client) = pair(a.clone(), b.clone());
        tokio::time::sleep(Duration::from_millis(170)).await;
        a.available.store(false, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(170)).await;
        a.copy(text_content("locked"));
        tokio::time::sleep(Duration::from_millis(170)).await;
        a.available.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(b.bytes(), b"b");
        a.copy(text_content("after unlock"));
        until(|| b.bytes() == b"after unlock").await;
        server.abort();
        client.abort();
    }
    #[tokio::test]
    async fn receiving_disabled_rejects_offers() {
        let (mut remote, stream) = tokio::io::duplex(1024);
        let task = tokio::spawn(exchange(
            stream,
            Memory::new("a"),
            Permissions {
                send: true,
                receive: false,
            },
            true,
        ));
        advertise(&mut remote, 1, 0, &text_content("no"))
            .await
            .unwrap();
        assert!(
            timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
    struct DeferredWrite {
        entered: AtomicBool,
        resume: AtomicBool,
        committed: AtomicBool,
    }
    impl Backend for DeferredWrite {
        fn available(&self) -> bool {
            true
        }
        fn version(&self) -> u64 {
            1
        }
        fn read(&self) -> Result<Option<Content>> {
            Ok(None)
        }
        fn write(&self, _: u64, _: &Content) -> Result<Option<u64>> {
            panic!("must use final authorization guard")
        }
        fn write_guarded(
            &self,
            _: u64,
            _: &Content,
            allowed: &(dyn Fn() -> bool + Sync),
        ) -> Result<Option<u64>> {
            self.entered.store(true, Ordering::Release);
            while !self.resume.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !allowed() {
                return Ok(None);
            }
            self.committed.store(true, Ordering::Release);
            Ok(Some(2))
        }
    }
    #[tokio::test]
    async fn cancelled_control_cannot_commit_a_delayed_native_write() {
        let native = Arc::new(DeferredWrite {
            entered: AtomicBool::new(false),
            resume: AtomicBool::new(false),
            committed: AtomicBool::new(false),
        });
        let host = Host::new(native.clone(), BOTH);
        let lease = host.lease().unwrap();
        let active = host.attach(&lease.token).unwrap();
        let backend = leased_backend(native.clone(), active);
        let work = tokio::task::spawn_blocking(move || backend.write(1, &text_content("delayed")));
        until(|| native.entered.load(Ordering::Acquire)).await;
        drop(lease);
        native.resume.store(true, Ordering::Release);
        assert!(work.await.unwrap().unwrap().is_none());
        assert!(!native.committed.load(Ordering::Acquire));
    }
}
