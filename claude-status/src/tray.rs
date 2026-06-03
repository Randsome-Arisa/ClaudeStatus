// 系统托盘管理
//
// 使用 tray-icon crate 创建和管理系统托盘图标。
// 图标图片从 assets/ 目录通过 include_bytes! 内嵌到二进制，
// 运行时不依赖外部文件。

use std::io::Cursor;
use std::sync::mpsc;

use anyhow::Result;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

use crate::ipc::IpcMessage;
use crate::state::DaemonState;

/// 编译时内嵌 4 个圆形状态图标 (32×32 RGBA PNG)
const ICON_IDLE: &[u8] = include_bytes!("../../assets/idle.png");
const ICON_WORKING: &[u8] = include_bytes!("../../assets/working.png");
const ICON_DONE: &[u8] = include_bytes!("../../assets/done.png");
const ICON_WAITING: &[u8] = include_bytes!("../../assets/waiting.png");

/// 将内嵌的 PNG 字节解码为 tray-icon::Icon
fn decode_png_icon(data: &[u8]) -> Result<Icon> {
    let decoder = png::Decoder::new(Cursor::new(data));
    // 确保输出为 RGBA8 格式
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    let width = info.width;
    let height = info.height;

    let mut buf = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut buf)?;

    Icon::from_rgba(buf, width, height)
        .map_err(|e| anyhow::anyhow!("图标创建失败: {}", e))
}

/// 托盘图标管理器
pub struct TrayManager {
    tray: TrayIcon,
    #[allow(dead_code)]
    quit_item_id: tray_icon::menu::MenuId,
    // 预加载的圆形图标
    icon_idle: Icon,
    icon_working: Icon,
    icon_done: Icon,
    icon_waiting: Icon,
}

impl TrayManager {
    /// 创建托盘图标并初始化
    ///
    /// `quit_tx` 用于在用户点击「退出」菜单项时向主循环发送退出信号。
    pub fn new(quit_tx: mpsc::Sender<IpcMessage>) -> Result<Self> {
        // Linux 上必须先初始化 GTK
        #[cfg(target_os = "linux")]
        {
            gtk::init().map_err(|e| anyhow::anyhow!("GTK 初始化失败: {}", e))?;
            log::info!("[DEBUG] GTK 初始化成功");
        }

        // 预加载所有圆形图标（解码 PNG → RGBA → Icon）
        let icon_idle = decode_png_icon(ICON_IDLE)?;
        let icon_working = decode_png_icon(ICON_WORKING)?;
        let icon_done = decode_png_icon(ICON_DONE)?;
        let icon_waiting = decode_png_icon(ICON_WAITING)?;
        log::info!("[DEBUG] 4 个圆形图标已内嵌加载");

        // 创建菜单
        let menu = Menu::new();
        let status_item = MenuItem::new("状态: 空闲", true, None);
        let quit_item = MenuItem::new("退出 ClaudeStatus", false, None);
        let separator = MenuItem::new("─────────────", true, None);

        menu.append(&status_item)
            .map_err(|e| anyhow::anyhow!("菜单项添加失败: {}", e))?;
        menu.append(&separator)
            .map_err(|e| anyhow::anyhow!("分隔线添加失败: {}", e))?;
        menu.append(&quit_item)
            .map_err(|e| anyhow::anyhow!("菜单项添加失败: {}", e))?;

        let quit_item_id = quit_item.id().clone();

        // 必须在 build() 之前注册 MenuEvent receiver
        let menu_channel = MenuEvent::receiver();
        let quit_id_for_thread = quit_item_id.clone();
        std::thread::spawn(move || {
            while let Ok(event) = menu_channel.recv() {
                log::info!("[DEBUG] 托盘菜单事件: {:?}", event.id);
                if event.id == quit_id_for_thread {
                    log::info!("[DEBUG] 用户点击退出菜单");
                    let _ = quit_tx.send(IpcMessage::Quit);
                }
            }
        });

        // 初始图标：灰色圆形（Idle 状态）
        let tray = TrayIconBuilder::new()
            .with_tooltip("ClaudeStatus: 空闲")
            .with_menu(Box::new(menu))
            .with_icon(icon_idle.clone())
            .build()
            .map_err(|e| anyhow::anyhow!("托盘图标创建失败: {}", e))?;

        log::info!("[DEBUG] 系统托盘图标已创建");

        Ok(TrayManager {
            tray,
            quit_item_id,
            icon_idle,
            icon_working,
            icon_done,
            icon_waiting,
        })
    }

    /// 根据状态更新托盘图标和工具提示
    pub fn update_state(&self, state: &DaemonState) {
        let icon = match state {
            DaemonState::Idle => &self.icon_idle,
            DaemonState::Working => &self.icon_working,
            DaemonState::Done => &self.icon_done,
            DaemonState::Waiting => &self.icon_waiting,
            DaemonState::Error(_) => &self.icon_idle,
        };
        let label = state.to_string();
        let tooltip = format!("ClaudeStatus: {}", label);

        if self.tray.set_icon(Some(icon.clone())).is_err() {
            log::warn!("[DEBUG] 托盘图标更新失败");
        }

        if self.tray.set_tooltip(Some(tooltip)).is_err() {
            log::warn!("[DEBUG] 托盘工具提示更新失败");
        }

        log::debug!(
            "[DEBUG] 托盘已更新: RGB({}, {}, {}) - {}",
            state.tray_color().0,
            state.tray_color().1,
            state.tray_color().2,
            label
        );
    }
}

impl Drop for TrayManager {
    fn drop(&mut self) {
        log::info!("[DEBUG] 系统托盘已清理");
    }
}
