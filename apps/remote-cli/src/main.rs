#[cfg(any(target_os = "macos", test))]
mod capture_readiness;
#[cfg(target_os = "macos")]
mod input_macos;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "windows", target_os = "macos", test))]
mod mouse;
mod pairing;
mod preview;
#[cfg(target_os = "macos")]
mod unlock_macos;
#[cfg(target_os = "macos")]
mod workspace_macos;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use remote_transport::{Identity, Socks5Proxy};
#[cfg(target_os = "macos")]
use std::sync::atomic::Ordering;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

use tokio::{net::TcpListener, sync::Semaphore};

#[derive(Parser)]
#[command(about = "MyRemoteApp 单窗口共享与鼠标控制（键盘尚未实现）")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 只读检查共享窗口的点击命中信息，不发送事件。
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    InputDiagnostics {
        #[arg(long)]
        window_id: u32,
        #[arg(long)]
        pid: i32,
        #[arg(long, default_value_t = 32767)]
        x: u16,
        #[arg(long, default_value_t = 55000)]
        y: u16,
    },
    /// 列出 Mac 当前可共享的 App 窗口及 ID。
    Windows,
    /// 启动 Mac 服务，并选择要共享的窗口。
    Serve {
        #[arg(long, default_value = "0.0.0.0:9443")]
        listen: String,
        #[arg(long, default_value = ".local/host")]
        state_dir: PathBuf,
        /// 重新生成连接凭据，让之前导出的连接文件失效。
        #[arg(long)]
        reset_pairing: bool,
        /// 指定共享窗口 ID；省略时显示列表供选择。
        #[arg(long, conflicts_with = "check_only")]
        window_id: Option<u32>,
        /// 将目标 App 移到独立 HiDPI 虚拟屏并放大；停止共享时尝试恢复窗口。
        #[arg(long, conflicts_with = "check_only")]
        virtual_display: bool,
        /// 只启动连接检查服务，不请求屏幕权限、不采集画面。
        #[arg(long)]
        check_only: bool,
        /// 仅共享画面，不启用鼠标控制；虚拟屏模式仍需辅助功能权限来移动窗口。
        #[arg(long, conflicts_with = "check_only")]
        view_only: bool,
    },
    /// 通过直连或 SOCKS5 打开共享窗口预览。
    #[command(alias = "probe")]
    Connect {
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "connect.json")]
        connection_file: PathBuf,
        /// SOCKS5 代理地址，例如 127.0.0.1:1080；目标域名由代理解析。
        #[arg(long)]
        socks5: Option<String>,
        #[arg(long, env = "REMOTEAPP_SOCKS5_USER", requires_all = ["socks5", "socks5_password"])]
        socks5_user: Option<String>,
        #[arg(long, env = "REMOTEAPP_SOCKS5_PASSWORD", hide_env_values = true,
              requires_all = ["socks5", "socks5_user"])]
        socks5_password: Option<String>,
        /// 仅检查连接，不打开预览窗口。
        #[arg(long, conflicts_with = "save_frame")]
        check_only: bool,
        /// 收取一帧并保存为 JPEG，不打开窗口，不覆盖已有文件。
        #[arg(long)]
        save_frame: Option<PathBuf>,
        /// 只观看画面，不建立鼠标控制连接。
        #[arg(long, conflicts_with_all = ["save_frame", "check_only"])]
        view_only: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        #[cfg(target_os = "macos")]
        Command::InputDiagnostics {
            window_id,
            pid,
            x,
            y,
        } => {
            input_macos::diagnose(window_id, pid, x, y)?;
        }
        Command::Windows => {
            #[cfg(target_os = "macos")]
            macos::list_windows()?;
            #[cfg(not(target_os = "macos"))]
            anyhow::bail!("请在 Mac 端运行 windows 命令选择共享窗口");
        }
        Command::Connect {
            address,
            connection_file,
            socks5,
            socks5_user,
            socks5_password,
            check_only,
            save_frame,
            view_only,
        } => {
            let connection = pairing::ConnectionFile::load(&connection_file)
                .with_context(|| format!("无法读取连接文件 {}", connection_file.display()))?;
            let identity = Identity::from_pem(
                connection.client_certificate.as_bytes(),
                connection.client_key.as_bytes(),
            )?;
            let config =
                remote_transport::client_config(identity, connection.host_certificate.as_bytes())?;
            let proxy = socks5.map(|address| Socks5Proxy {
                address: address
                    .strip_prefix("socks5://")
                    .unwrap_or(&address)
                    .to_owned(),
                credentials: socks5_user.zip(socks5_password),
            });
            let mut stream =
                remote_transport::connect_via(&address, config.clone(), proxy.as_ref()).await?;
            if check_only {
                let rtt = remote_session::probe(&mut stream).await?;
                println!(
                    "{}连接成功，往返延迟 {:.2} ms。",
                    if proxy.is_some() { "SOCKS5 " } else { "直连" },
                    rtt.as_secs_f64() * 1000.0
                );
            } else {
                let (locked, available) = remote_session::remote_status(&mut stream).await?;
                drop(stream);
                if locked {
                    anyhow::ensure!(
                        available,
                        "Mac 已锁屏且当前服务不能远程解锁；请在 Mac 授予辅助功能权限或本机解锁"
                    );
                    println!("Mac 已锁屏。请输入该 Mac 用户的登录密码，仅用于本次解锁，不会保存。");
                    let password = tokio::task::spawn_blocking(|| {
                        rpassword::prompt_password("Mac 登录密码：").map(zeroize::Zeroizing::new)
                    })
                    .await
                    .context("密码输入已取消")?
                    .context("无法读取密码，请在交互式终端运行")?;
                    let mut unlock_stream =
                        remote_transport::connect_via(&address, config.clone(), proxy.as_ref())
                            .await?;
                    remote_session::unlock_remote(&mut unlock_stream, password).await?;
                    println!("Mac 已解锁，正在连接原共享窗口。");
                }
                let mut stream =
                    remote_transport::connect_via(&address, config.clone(), proxy.as_ref()).await?;
                if let Some(path) = save_frame {
                    preview::save_frame(&mut stream, &path).await?;
                    return Ok(());
                }
                let control = if view_only {
                    None
                } else {
                    Some(remote_transport::connect_via(&address, config, proxy.as_ref()).await?)
                };
                preview::view(stream, control).await?;
            }
        }
        Command::Serve {
            listen,
            state_dir,
            reset_pairing,
            window_id,
            virtual_display,
            check_only,
            view_only,
        } => {
            let listener = TcpListener::bind(&listen).await?;
            #[cfg(target_os = "macos")]
            let mut capture = if check_only {
                None
            } else {
                Some(macos::start(window_id, view_only, virtual_display)?)
            };
            #[cfg(target_os = "macos")]
            let mut preview = capture.as_ref().map(|capture| capture.source.clone());
            #[cfg(not(target_os = "macos"))]
            let preview = {
                let _ = (window_id, view_only, virtual_display);
                anyhow::ensure!(
                    check_only,
                    "窗口采集服务只能在 Mac 端启动；连接检查可用 --check-only"
                );
                None
            };
            let ready = Arc::new(AtomicBool::new(false));
            #[cfg(target_os = "macos")]
            let recovery_requested = Arc::new(AtomicBool::new(false));
            #[cfg(target_os = "macos")]
            let unlock: Option<Arc<dyn remote_session::UnlockHost>> =
                Some(Arc::new(unlock_macos::NativeUnlockHost::new(
                    !check_only && !view_only,
                    ready.clone(),
                    recovery_requested.clone(),
                )));
            #[cfg(not(target_os = "macos"))]
            let unlock: Option<Arc<dyn remote_session::UnlockHost>> = None;
            let state = pairing::load_or_create(&state_dir, reset_pairing)?;
            let config = remote_transport::server_config(
                Identity::from_pem(
                    state.connection.host_certificate.as_bytes(),
                    state.host_key.as_bytes(),
                )?,
                state.connection.client_certificate.as_bytes(),
            )?;
            let slots = Arc::new(Semaphore::new(8));
            println!("服务已启动：{}（Ctrl-C 停止）", listener.local_addr()?);
            println!(
                "把 {} 复制到客户端当前目录即可，无需手动配置证书。",
                state_dir.join("connect.json").display()
            );
            println!("连接文件相当于访问密码；--reset-pairing 可让旧文件失效。");
            if check_only {
                println!("当前仅检查连接，未共享窗口。");
            }
            let mut maintenance = tokio::time::interval(Duration::from_secs(1));
            let shutdown = shutdown_signal();
            tokio::pin!(shutdown);
            #[cfg(target_os = "macos")]
            let mut resolution_error: Option<String> = None;
            #[cfg(target_os = "macos")]
            let mut capture_readiness = capture_readiness::CaptureReadiness::default();
            loop {
                let (tcp, address) = tokio::select! {
                    result = listener.accept() => result?,
                    result = &mut shutdown => { result?; break; }
                    _=maintenance.tick()=>{
                        #[cfg(target_os = "macos")]
                        if let Some(current)=&capture {
                            if macos::is_locked() {
                                ready.store(false,Ordering::Release);
                                recovery_requested.store(true,Ordering::Release);
                                capture_readiness.reset();
                                current.pause_for_lock();
                            } else if recovery_requested.swap(false,Ordering::AcqRel) || current.paused_for_lock() || matches!(&*current.source.frames.borrow(),Some(remote_session::CaptureEvent::Stopped(_))) {
                                ready.store(false,Ordering::Release);
                                capture_readiness.reset();
                                match current.resume(view_only) {
                                    Ok(resumed)=>{
                                        preview=Some(resumed.source.clone());capture=Some(resumed);
                                        println!("Mac 已解锁，正在等待原共享窗口画面恢复。");
                                    }
                                    Err(_)=>{ recovery_requested.store(true,Ordering::Release); }
                                }
                            } else {
                                let has_frame=matches!(&*current.source.frames.borrow(),Some(remote_session::CaptureEvent::Frame(_)));
                                let healthy=has_frame && !current.paused_for_lock() && !macos::is_locked();
                                let recovered=capture_readiness.observe(healthy,std::time::Instant::now());
                                let was_ready=ready.swap(recovered,Ordering::AcqRel);
                                if recovered && !was_ready {
                                    println!("原共享窗口画面已恢复，可以连接。");
                                }
                                if has_frame {
                                match current.refresh_resolution() {
                                    Ok(()) => { resolution_error = None; }
                                    Err(error) => {
                                        let message = format!("{error:#}");
                                        if resolution_error.as_ref() != Some(&message) {
                                            eprintln!("共享分辨率检查失败：{message}");
                                            resolution_error = Some(message);
                                        }
                                    }
                                }
                                }
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        let _=&ready;
                        continue;
                    }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let config = config.clone();
                let preview = preview.clone();
                let unlock = unlock.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let mut stream = remote_transport::accept(tcp, config).await?;
                        remote_session::serve_with_host(&mut stream, preview, unlock).await
                    }
                    .await;
                    match result {
                        Ok(()) => println!("{address}：会话结束"),
                        Err(error) => eprintln!("{address}：{error:#}"),
                    }
                });
            }
        }
    }
    Ok(())
}

// Keep SIGTERM graceful too, so stopping a background service can restore its
// window and release the virtual display before Tokio shuts down.
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    #[test]
    fn connection_check_cannot_create_a_virtual_display() {
        assert!(
            Cli::try_parse_from(["remote-cli", "serve", "--check-only", "--virtual-display"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["remote-cli", "serve", "--check-only"]).is_ok());
    }

    #[test]
    fn virtual_display_can_be_used_without_mouse_control() {
        let cli = Cli::try_parse_from([
            "remote-cli",
            "serve",
            "--virtual-display",
            "--view-only",
            "--window-id",
            "123",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Serve {
                virtual_display: true,
                view_only: true,
                window_id: Some(123),
                ..
            }
        ));
    }
}
