// 状态机定义与转换逻辑
//
// 管理守护进程的全局状态，接收 HookEvent 并输出新状态。
// 编译期穷举检查确保所有状态转换都被覆盖。

use std::fmt;

/// 守护进程状态
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonState {
    /// 空闲 — Claude Code 没有在执行任务
    Idle,
    /// 工作中 — Claude 正在调用工具
    Working,
    /// 已完成 — Claude 已完成当前任务
    Done,
    /// 等待授权 — Claude 在等待用户 PermissionRequest 响应
    Waiting,
    /// 错误 — IPC 或 JSON 解析出错
    Error(String),
}

impl fmt::Display for DaemonState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DaemonState::Idle => write!(f, "空闲"),
            DaemonState::Working => write!(f, "工作中"),
            DaemonState::Done => write!(f, "已完成"),
            DaemonState::Waiting => write!(f, "等待授权"),
            DaemonState::Error(msg) => write!(f, "错误: {}", msg),
        }
    }
}

impl DaemonState {
    /// 返回状态对应的托盘颜色 (R, G, B)
    pub fn tray_color(&self) -> (u8, u8, u8) {
        match self {
            DaemonState::Idle => (128, 128, 128),   // ⚫ 灰色
            DaemonState::Working => (0, 255, 0),     // 🟢 绿色
            DaemonState::Done => (255, 50, 50),       // 🔴 红色
            DaemonState::Waiting => (255, 200, 0),    // 🟡 黄色
            DaemonState::Error(_) => (128, 128, 128), // ⚫ 灰色
        }
    }

    /// 是否需要播放提示音（Working 状态时间长，不需要声音通知）
    pub fn needs_sound(&self) -> bool {
        matches!(self, DaemonState::Done | DaemonState::Waiting)
    }

    /// 是否需要发送桌面通知
    pub fn needs_notification(&self) -> bool {
        matches!(self, DaemonState::Done | DaemonState::Waiting | DaemonState::Error(_))
    }
}

/// IPC 传入的 Hook 事件
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "event")]
pub enum HookEvent {
    /// UserPromptSubmit 或 PreToolUse 触发 → Claude 开始工作/思考
    #[serde(rename = "working")]
    Working,
    /// Stop 触发 → Claude 完成当前任务
    #[serde(rename = "done")]
    Done,
    /// PermissionRequest 触发 → Claude 等待用户授权
    #[serde(rename = "waiting")]
    Waiting,
    /// PostToolUse 触发 → 用户已授权，Claude 继续工作
    #[serde(rename = "resumed")]
    Resumed,
    /// 未知事件或解析错误
    #[serde(other)]
    Unknown,
}

/// 状态转换（纯函数，无副作用）
///
/// 状态转换表：
///
/// | 当前状态 | 触发事件      | 新状态   |
/// |----------|--------------|----------|
/// | Idle     | Working      | Working  |
/// | Working  | Done         | Done     |
/// | Working  | Waiting      | Waiting  |
/// | Waiting  | Resumed      | Working  |
/// | Done     | Working      | Working  |
/// | Any      | Unknown      | 不变     |
/// | Any      | Error(解析)  | Error    |
pub fn transition(current: &DaemonState, event: &HookEvent) -> DaemonState {
    match (current, event) {
        // IDLE → WORKING
        (DaemonState::Idle, HookEvent::Working) => DaemonState::Working,

        // WORKING → DONE
        (DaemonState::Working, HookEvent::Done) => DaemonState::Done,

        // WORKING → WAITING
        (DaemonState::Working, HookEvent::Waiting) => DaemonState::Waiting,

        // WAITING → WORKING (用户已授权或继续操作)
        (DaemonState::Waiting, HookEvent::Resumed) => DaemonState::Working,
        (DaemonState::Waiting, HookEvent::Working) => DaemonState::Working,

        // WAITING → DONE (用户忽略权限请求，session 结束)
        (DaemonState::Waiting, HookEvent::Done) => DaemonState::Done,

        // DONE → WORKING
        (DaemonState::Done, HookEvent::Working) => DaemonState::Working,

        // ERROR → WORKING
        (DaemonState::Error(_), HookEvent::Working) => DaemonState::Working,

        // UNKNOWN 或无效转换：保持当前状态不变
        (_, HookEvent::Unknown) => current.clone(),
        _ => current.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idle_to_working_on_pretooluse() {
        let result = transition(&DaemonState::Idle, &HookEvent::Working);
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_working_to_done_on_stop() {
        let result = transition(&DaemonState::Working, &HookEvent::Done);
        assert_eq!(result, DaemonState::Done);
    }

    #[test]
    fn test_working_to_waiting_on_permission_request() {
        let result = transition(&DaemonState::Working, &HookEvent::Waiting);
        assert_eq!(result, DaemonState::Waiting);
    }

    #[test]
    fn test_waiting_to_working_on_posttooluse() {
        let result = transition(&DaemonState::Waiting, &HookEvent::Resumed);
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_waiting_to_working_on_pretooluse() {
        // Waiting 状态下收到 Working 事件，也应该切换到 Working
        let result = transition(&DaemonState::Waiting, &HookEvent::Working);
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_done_to_working_on_pretooluse() {
        let result = transition(&DaemonState::Done, &HookEvent::Working);
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_invalid_json_stays_unchanged() {
        // Unknown 事件保持当前状态不变
        let result = transition(&DaemonState::Working, &HookEvent::Unknown);
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_error_to_working_on_pretooluse() {
        let result = transition(
            &DaemonState::Error("parse error".into()),
            &HookEvent::Working,
        );
        assert_eq!(result, DaemonState::Working);
    }

    #[test]
    fn test_waiting_to_done_on_stop() {
        // 用户忽略权限请求，session 直接结束
        let result = transition(&DaemonState::Waiting, &HookEvent::Done);
        assert_eq!(result, DaemonState::Done);
    }

    #[test]
    fn test_unknown_event_keeps_idle() {
        let result = transition(&DaemonState::Idle, &HookEvent::Unknown);
        assert_eq!(result, DaemonState::Idle);
    }
}
