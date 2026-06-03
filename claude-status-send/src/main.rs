// claude-status-send — IPC 事件发送器
//
// 向 ClaudeStatus 守护进程发送 JSON 事件或命令。
// 用于 Claude Code hook 脚本中替代 `nc`（Windows 上 `nc` 不可用）。
//
// 用法:
//   claude-status-send --event working
//   claude-status-send --command quit

use anyhow::{Context, Result};
use clap::Parser;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, traits::Stream};
use std::io::Write;

#[derive(Parser)]
#[command(
    name = "claude-status-send",
    version,
    about = "向 ClaudeStatus 守护进程发送事件"
)]
struct Cli {
    /// 发送 Hook 事件: working, done, waiting, resumed
    #[arg(short, long)]
    event: Option<String>,

    /// 发送管理命令: quit
    #[arg(short, long)]
    command: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 构造 JSON（手动格式化以消除 serde_json 依赖，缩小二进制体积）
    let json = match (&cli.event, &cli.command) {
        (Some(event), _) => {
            // 用最简单的字符串拼接，event 参数已经过 clap 校验
            format!(r#"{{"event":"{}"}}"#, event)
        }
        (_, Some(cmd)) => {
            format!(r#"{{"command":"{}"}}"#, cmd)
        }
        (None, None) => {
            eprintln!("错误: 需要 --event 或 --command 参数");
            eprintln!("示例: claude-status-send --event working");
            std::process::exit(1);
        }
    };

    // 平台 IPC 路径
    #[cfg(target_os = "windows")]
    let socket_path: String = r"\\.\pipe\claude-status".to_string();
    #[cfg(not(target_os = "windows"))]
    let socket_path: String = "/tmp/claude-status.sock".to_string();

    // 连接并写入
    let name = socket_path
        .clone()
        .to_fs_name::<GenericFilePath>()
        .with_context(|| format!("无效的 IPC 路径: {}", socket_path))?;

    let mut conn = interprocess::local_socket::Stream::connect(name)
        .with_context(|| format!("无法连接到守护进程 ({}), 守护进程可能未运行", socket_path))?;

    conn.write_all(json.as_bytes())
        .with_context(|| "写入失败")?;
    conn.write_all(b"\n")
        .with_context(|| "写入失败")?;
    conn.flush()
        .with_context(|| "刷新失败")?;

    Ok(())
}
