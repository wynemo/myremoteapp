#[cfg(target_os = "macos")]
mod macos;
mod pairing;
mod preview;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use remote_transport::{Identity, Socks5Proxy};
use std::{path::PathBuf, sync::Arc};
use tokio::{net::TcpListener, sync::Semaphore};

#[derive(Parser)]
#[command(about = "MyRemoteApp 单窗口预览（键鼠控制尚未实现）")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
        /// 只启动连接检查服务，不请求屏幕权限、不采集画面。
        #[arg(long)]
        check_only: bool,
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
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
                remote_transport::connect_via(&address, config, proxy.as_ref()).await?;
            if check_only {
                let rtt = remote_session::probe(&mut stream).await?;
                println!(
                    "{}连接成功，往返延迟 {:.2} ms。",
                    if proxy.is_some() { "SOCKS5 " } else { "直连" },
                    rtt.as_secs_f64() * 1000.0
                );
            } else if let Some(path) = save_frame {
                preview::save_frame(&mut stream, &path).await?;
            } else {
                preview::view(stream).await?;
            }
        }
        Command::Serve {
            listen,
            state_dir,
            reset_pairing,
            window_id,
            check_only,
        } => {
            let listener = TcpListener::bind(&listen).await?;
            #[cfg(target_os = "macos")]
            let capture = if check_only {
                None
            } else {
                Some(macos::start(window_id)?)
            };
            #[cfg(target_os = "macos")]
            let preview = capture.as_ref().map(|capture| capture.source.clone());
            #[cfg(not(target_os = "macos"))]
            let preview = {
                let _ = window_id;
                anyhow::ensure!(
                    check_only,
                    "窗口采集服务只能在 Mac 端启动；连接检查可用 --check-only"
                );
                None
            };
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
                "把 {} 复制到 Windows 的 remote-cli.exe 旁边即可，无需手动配置证书。",
                state_dir.join("connect.json").display()
            );
            println!("连接文件相当于访问密码；--reset-pairing 可让旧文件失效。");
            if check_only {
                println!("当前仅检查连接，未共享窗口。");
            }
            loop {
                let (tcp, address) = tokio::select! {
                    result = listener.accept() => result?,
                    result = tokio::signal::ctrl_c() => { result?; break; }
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue;
                };
                let config = config.clone();
                let preview = preview.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = async {
                        let mut stream = remote_transport::accept(tcp, config).await?;
                        remote_session::serve_with_preview(&mut stream, preview).await
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
