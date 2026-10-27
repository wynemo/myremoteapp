mod pairing;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use remote_transport::{Identity, Socks5Proxy};
use std::{path::PathBuf, sync::Arc};
use tokio::{net::TcpListener, sync::Semaphore};

#[derive(Parser)]
#[command(about = "MyRemoteApp 连接检查工具（窗口控制尚未实现）")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 启动 Mac 连接检查服务，自动生成供 Windows 导入的 connect.json。
    Serve {
        #[arg(long, default_value = "0.0.0.0:9443")]
        listen: String,
        #[arg(long, default_value = ".local/host")]
        state_dir: PathBuf,
        /// 重新生成连接凭据，让之前导出的连接文件失效。
        #[arg(long)]
        reset_pairing: bool,
    },
    /// 通过直连或 SOCKS5 检查连接；不传输画面或控制输入。
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Connect {
            address,
            connection_file,
            socks5,
            socks5_user,
            socks5_password,
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
            let rtt = remote_session::probe(&mut stream).await?;
            println!(
                "{}连接成功，往返延迟 {:.2} ms。",
                if proxy.is_some() { "SOCKS5 " } else { "直连" },
                rtt.as_secs_f64() * 1000.0
            );
            println!("当前仅验证连接，窗口画面与远程控制尚未实现。");
        }
        Command::Serve {
            listen,
            state_dir,
            reset_pairing,
        } => {
            let listener = TcpListener::bind(&listen).await?;
            let state = pairing::load_or_create(&state_dir, reset_pairing)?;
            let config = remote_transport::server_config(
                Identity::from_pem(
                    state.connection.host_certificate.as_bytes(),
                    state.host_key.as_bytes(),
                )?,
                state.connection.client_certificate.as_bytes(),
            )?;
            let slots = Arc::new(Semaphore::new(8));
            println!(
                "连接检查服务已启动：{}（Ctrl-C 停止）",
                listener.local_addr()?
            );
            println!(
                "把 {} 复制到 Windows 的 remote-cli.exe 旁边即可，无需手动配置证书。",
                state_dir.join("connect.json").display()
            );
            println!("连接文件相当于访问密码；--reset-pairing 可让旧文件失效。");
            println!("当前仅验证连接，窗口画面与远程控制尚未实现。");
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
                        Ok(()) => println!("{address}：连接检查通过"),
                        Err(error) => eprintln!("{address}：{error:#}"),
                    }
                });
            }
        }
    }
    Ok(())
}
