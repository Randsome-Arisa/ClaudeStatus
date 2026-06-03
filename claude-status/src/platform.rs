// 跨平台抽象层
//
// 集中管理 Linux/Windows 差异：
// - IPC socket 路径
// - IPC listener 类型
// - 平台特定逻辑

/// IPC socket 路径
///
/// Linux: Unix Domain Socket 在 /tmp/
/// Windows: Named Pipe 在 \\.\pipe\
#[cfg(target_os = "linux")]
pub fn socket_path() -> String {
    "/tmp/claude-status.sock".to_string()
}

#[cfg(target_os = "windows")]
pub fn socket_path() -> String {
    r"\\.\pipe\claude-status".to_string()
}

/// 检查平台是否是 Linux
pub const fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

/// 检查平台是否是 Windows
pub const fn is_windows() -> bool {
    cfg!(target_os = "windows")
}
