// IPC 通信层
//
// 通过 Unix Domain Socket (Linux) 或 Named Pipe (Windows) 接收
// Claude Code hook 事件和 daemon 管理命令。
// 协议：单行 JSON，`\n` 分隔。
//
// 使用 `interprocess` crate 实现跨平台本地 socket 通信。

use crate::state::HookEvent;
use anyhow::{Context, Result};
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use std::io::BufRead;
use std::io::BufReader;

/// IPC 消息类型（事件 + 命令）
#[derive(Debug, Clone)]
pub enum IpcMessage {
    /// Claude Code hook 事件
    Event(HookEvent),
    /// 退出守护进程
    Quit,
}

/// 启动 IPC listener，阻塞等待连接并逐行处理消息。
///
/// 在独立线程中运行，通过 mpsc channel 发送解析后的消息。
pub fn start_listener(
    socket_path: &str,
    tx: std::sync::mpsc::Sender<IpcMessage>,
) -> Result<()> {
    // 如果旧 socket 文件残留（上次崩溃），先删除
    let path = std::path::Path::new(socket_path);
    if path.exists() {
        log::warn!("[DEBUG] 发现残留 socket 文件，正在清理: {}", socket_path);
        std::fs::remove_file(path)
            .with_context(|| format!("无法删除残留 socket: {}", socket_path))?;
    }

    let name = socket_path.to_fs_name::<GenericFilePath>()?;

    let listener = ListenerOptions::new()
        .name(name)
        .create_sync()
        .with_context(|| format!("无法创建 IPC listener: {}", socket_path))?;

    log::info!("[DEBUG] IPC listener 已启动: {}", socket_path);

    // 接受连接循环
    for stream_result in listener.incoming() {
        match stream_result {
            Ok(stream) => {
                log::debug!("[DEBUG] IPC 新连接已建立");

                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    match line {
                        Ok(line) if line.trim().is_empty() => continue,
                        Ok(line) => {
                            log::debug!("[DEBUG] IPC 收到: {}", line.trim());
                            match parse_message(&line) {
                                Ok(msg) => {
                                    let is_quit = matches!(msg, IpcMessage::Quit);
                                    if let Err(e) = tx.send(msg) {
                                        log::error!(
                                            "[DEBUG] 发送消息到 channel 失败: {}",
                                            e
                                        );
                                        break;
                                    }
                                    if is_quit {
                                        log::info!("[DEBUG] 收到 quit 命令，listener 退出");
                                        return Ok(());
                                    }
                                }
                                Err(e) => {
                                    log::warn!(
                                        "[DEBUG] 消息解析失败: {} | 原始数据: {}",
                                        e,
                                        line.trim()
                                    );
                                    let _ = tx.send(IpcMessage::Event(HookEvent::Unknown));
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("[DEBUG] IPC 读取错误: {}", e);
                            break;
                        }
                    }
                }
                log::debug!("[DEBUG] IPC 连接关闭");
            }
            Err(e) => {
                log::error!("[DEBUG] IPC 接受连接失败: {}", e);
            }
        }
    }

    Ok(())
}

/// 解析 IPC JSON 消息：支持事件（`{"event":"done"}`）和命令（`{"command":"quit"}`）
fn parse_message(line: &str) -> Result<IpcMessage> {
    let line = line.trim();

    // 先快速检查是否为命令（有 "command" 字段）
    let raw: serde_json::Value =
        serde_json::from_str(line).context("JSON 解析失败")?;

    if let Some(cmd) = raw.get("command").and_then(|v| v.as_str()) {
        match cmd {
            "quit" => return Ok(IpcMessage::Quit),
            other => {
                log::warn!("[DEBUG] 未知命令: {}", other);
                return Ok(IpcMessage::Event(HookEvent::Unknown));
            }
        }
    }

    // 解析为 Hook 事件
    if raw.get("event").is_some() {
        let event: HookEvent =
            serde_json::from_str(line).context("无法解析 Hook 事件")?;
        return Ok(IpcMessage::Event(event));
    }

    // JSON 格式正确但没有 event 或 command 字段 → 忽略
    Ok(IpcMessage::Event(HookEvent::Unknown))
}

/// 连接到 daemon 的 IPC socket 并发送一行 JSON
pub fn send_to_daemon(socket_path: &str, json: &str) -> Result<()> {
    use interprocess::local_socket::traits::Stream;
    use std::io::Write;

    let name = socket_path.to_fs_name::<GenericFilePath>()?;
    let mut conn = interprocess::local_socket::Stream::connect(name)
        .with_context(|| format!("无法连接到守护进程: {}", socket_path))?;

    conn.write_all(json.as_bytes())
        .with_context(|| "写入 IPC 消息失败")?;
    conn.write_all(b"\n")
        .with_context(|| "写入 IPC 换行符失败")?;
    conn.flush()
        .with_context(|| "刷新 IPC 连接失败")?;

    Ok(())
}

/// 解析单行 JSON 为 HookEvent（测试用）
#[cfg(test)]
fn parse_event(line: &str) -> Result<HookEvent> {
    let event: HookEvent =
        serde_json::from_str(line.trim()).context("无法解析 Hook 事件 JSON")?;
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    // === parse_message 测试 ===

    #[test]
    fn test_parse_message_quit() {
        let msg = parse_message(r#"{"command":"quit"}"#).unwrap();
        assert!(matches!(msg, IpcMessage::Quit));
    }

    #[test]
    fn test_parse_message_event_done() {
        let msg = parse_message(r#"{"event":"done"}"#).unwrap();
        assert!(matches!(msg, IpcMessage::Event(HookEvent::Done)));
    }

    // === parse_event 测试 ===

    #[test]
    fn test_parse_valid_json_event() {
        let json = r#"{"event":"done"}"#;
        let event = parse_event(json).unwrap();
        assert_eq!(event, HookEvent::Done);
    }

    #[test]
    fn test_parse_valid_working_event() {
        let json = r#"{"event":"working"}"#;
        let event = parse_event(json).unwrap();
        assert_eq!(event, HookEvent::Working);
    }

    #[test]
    fn test_parse_valid_waiting_event() {
        let json = r#"{"event":"waiting"}"#;
        let event = parse_event(json).unwrap();
        assert_eq!(event, HookEvent::Waiting);
    }

    #[test]
    fn test_parse_invalid_json_returns_error() {
        let json = "{bad";
        let result = parse_event(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_unknown_event_is_unknown() {
        let json = r#"{"event":"something_else"}"#;
        let event = parse_event(json).unwrap();
        assert_eq!(event, HookEvent::Unknown);
    }

    #[test]
    fn test_parse_empty_json_returns_error() {
        let json = "";
        let result = parse_event(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_message_unknown_command() {
        let msg = parse_message(r#"{"command":"foobar"}"#).unwrap();
        assert!(matches!(msg, IpcMessage::Event(HookEvent::Unknown)));
    }

    #[test]
    fn test_parse_message_no_event_or_command() {
        let msg = parse_message(r#"{"foo":"bar"}"#).unwrap();
        assert!(matches!(msg, IpcMessage::Event(HookEvent::Unknown)));
    }
}
