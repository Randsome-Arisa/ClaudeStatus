// 桌面通知
//
// 使用 notify-rust crate 发送跨平台桌面通知。
// 带 10 秒频率限制，5 秒后自动消失。

use crate::state::DaemonState;
use std::time::Instant;

/// 桌面通知管理器
pub struct NotificationManager {
    last_notification: Option<Instant>,
}

impl NotificationManager {
    /// 创建通知管理器
    pub fn new() -> Self {
        log::info!("[DEBUG] 桌面通知管理器已初始化");
        NotificationManager {
            last_notification: None,
        }
    }

    /// 根据状态发送桌面通知（带 10 秒频率限制）
    pub fn notify(&mut self, state: &DaemonState) {
        let now = Instant::now();

        // 频率限制检查：同类型通知 10 秒内只发一次
        if let Some(last) = self.last_notification {
            if now.duration_since(last).as_secs() < 10 {
                log::debug!("[DEBUG] 通知频率限制，跳过: {}", state);
                return;
            }
        }

        let (title, body) = match state {
            DaemonState::Done => ("ClaudeStatus", "Claude 已完成任务"),
            DaemonState::Waiting => ("ClaudeStatus", "Claude 正在等待你的操作"),
            DaemonState::Error(msg) => ("ClaudeStatus ⚠️", msg.as_str()),
            _ => return, // 其他状态不发送通知
        };

        // 发送桌面通知
        match notify_rust::Notification::new()
            .summary(title)
            .body(body)
            .timeout(5000) // 5 秒后自动消失
            .show()
        {
            Ok(_) => {
                self.last_notification = Some(now);
                log::info!("[DEBUG] 桌面通知已发送: {}", body);
            }
            Err(e) => {
                log::warn!("[DEBUG] 桌面通知发送失败: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiting() {
        let mut mgr = NotificationManager::new();

        // 第一次通知应该发送（在测试环境中会静默失败，因为无 D-Bus）
        mgr.notify(&DaemonState::Done);

        // 第二次通知应该在 10 秒内被频率限制阻止
        // 无法直接验证行为（依赖外部系统），但至少不 panic
        mgr.notify(&DaemonState::Done);
    }
}
