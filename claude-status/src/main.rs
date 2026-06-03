// ClaudeStatus — 系统托盘状态指示器
//
// 守护进程 + CLI 入口点。
// 使用 clap 解析子命令，mpsc channel 串行化 IPC 事件，
// 状态机驱动托盘颜色、声音、桌面通知。

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};

mod config;
mod install;
mod ipc;
mod notify;
mod platform;
mod sound;
mod state;
mod tray;

use ipc::IpcMessage;
use state::DaemonState;

/// Claude Code 桌面状态提示工具 — 系统托盘 + 声音 + 通知
#[derive(Parser)]
#[command(
    name = "claude-status",
    version,
    about = "Claude Code CLI 桌面状态提示工具"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动后台守护进程
    Daemon {
        /// 详细日志输出
        #[arg(short, long)]
        verbose: bool,
        /// 日志文件路径
        #[arg(long)]
        log_file: Option<String>,
    },
    /// 停止守护进程
    Quit,
    /// 打印当前状态 (JSON)
    Status {
        /// 持续监控模式（终端文本输出）
        #[arg(short, long)]
        watch: bool,
    },
    /// 自动配置 ~/.claude/settings.json hooks
    Install,
    /// 移除 hooks 配置并停止守护进程
    Uninstall,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon { verbose, log_file } => run_daemon(verbose, log_file),
        Commands::Quit => send_quit_command(),
        Commands::Status { watch } => {
            if watch {
                watch_status()
            } else {
                print_status()
            }
        }
        Commands::Install => install::install_hooks(),
        Commands::Uninstall => install::uninstall_hooks(),
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Daemon Lifecycle
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 启动守护进程：IPC listener → event loop → 状态机
fn run_daemon(verbose: bool, log_file: Option<String>) -> Result<()> {
    config::init_logging(verbose, log_file.as_deref())?;

    let socket_path = platform::socket_path();
    log::info!("[DEBUG] ClaudeStatus daemon v{} 启动中...", env!("CARGO_PKG_VERSION"));
    log::info!("[DEBUG] IPC socket: {}", socket_path);

    // 守护进程状态文件路径
    let state_file = state_file_path();

    // 创建 channel：IPC 线程 → 主循环
    let (tx, rx) = mpsc::channel::<IpcMessage>();

    // 初始化托盘管理器（必须先于 IPC listener）
    // 传入 tx 以便用户点击「退出」菜单时能发送 quit 信号到主循环
    let tray_mgr = tray::TrayManager::new(tx.clone())?;
    log::info!("[DEBUG] 系统托盘已就绪");

    // 处理 GTK 事件以确保托盘图标通过 D-Bus 注册到 GNOME Shell
    // libayatana-appindicator3 使用 GDBus 通信，需要迭代 GLib 主循环
    // 才能响应面板的属性查询并显示图标。
    // 短暂迭代几次即可完成初始 D-Bus 注册往返。
    #[cfg(target_os = "linux")]
    {
        for i in 0..10 {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(Duration::from_millis(50));
            if i == 0 {
                log::info!("[DEBUG] GTK 事件循环已启动");
            }
        }
        log::info!("[DEBUG] GTK 初始事件处理完成");
    }

    // 初始化通知管理器
    let mut notif_mgr = notify::NotificationManager::new();

    // 初始化音频播放器
    let sound_player = sound::SoundPlayer::new()?;

    // 初始状态
    let mut state = DaemonState::Idle;
    let mut state_since = Instant::now();
    write_state_file(&state_file, &state);

    update_tray(&tray_mgr, &state);
    write_state_file(&state_file, &state);

    // 在独立线程启动 IPC listener（在所有资源初始化完成后才接受连接）
    let socket_clone = socket_path.clone();
    let ipc_handle = thread::spawn(move || {
        if let Err(e) = ipc::start_listener(&socket_clone, tx) {
            log::error!("[DEBUG] IPC listener 异常退出: {}", e);
        }
    });

    log::info!("[DEBUG] 守护进程就绪，监听 IPC 事件...");

    // ─── 主事件循环 ───
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(IpcMessage::Quit) => {
                log::info!("[DEBUG] 收到 quit 命令，正在退出...");
                break;
            }
            Ok(IpcMessage::Event(event)) => {
                let new_state = state::transition(&state, &event);

                if new_state != state {
                    log::info!(
                        "[DEBUG] 状态转换: {} → {} (事件: {:?})",
                        state,
                        new_state,
                        event
                    );

                    state = new_state;
                    state_since = Instant::now();

                    // 驱动输出
                    update_tray(&tray_mgr, &state);
                    write_state_file(&state_file, &state);

                    if state.needs_sound() {
                        match state {
                            DaemonState::Done => {
                                sound_player.play(sound::SoundEvent::Done)
                            }
                            DaemonState::Waiting => {
                                sound_player.play(sound::SoundEvent::Waiting)
                            }
                            _ => {}
                        }
                    }

                    if state.needs_notification() {
                        notif_mgr.notify(&state);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 检查超时状态转换
                let elapsed = state_since.elapsed();

                match &state {
                    DaemonState::Done if elapsed >= Duration::from_secs(60) => {
                        log::info!("[DEBUG] 超时转换: DONE → IDLE (60s 无事件)");
                        state = DaemonState::Idle;
                        state_since = Instant::now();
                        update_tray(&tray_mgr, &state);
                        write_state_file(&state_file, &state);
                    }
                    DaemonState::Error(_) if elapsed >= Duration::from_secs(30) => {
                        log::info!("[DEBUG] 超时转换: ERROR → IDLE (30s 无事件)");
                        state = DaemonState::Idle;
                        state_since = Instant::now();
                        update_tray(&tray_mgr, &state);
                        write_state_file(&state_file, &state);
                    }
                    _ => {}
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                log::error!("[DEBUG] IPC channel 断开，守护进程退出");
                break;
            }
        }

        // 每次事件/超时后处理平台事件：
        // - Linux: GTK 事件循环（D-Bus 通信 + 菜单点击）
        // - Windows: 窗口消息泵（Shell_NotifyIcon 消息 + 菜单点击 + Taskbar 重启）
        #[cfg(target_os = "linux")]
        {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
        }
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                PeekMessageW, DispatchMessageW, TranslateMessage, PM_REMOVE,
            };
            let mut msg = unsafe { std::mem::zeroed() };
            unsafe {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }

    // ─── 优雅退出 ───
    log::info!("[DEBUG] 守护进程正在退出...");

    // 等待 IPC 线程退出（give it a moment）
    let _ = ipc_handle.join();

    // 清理状态文件
    let _ = std::fs::remove_file(&state_file);

    // 清理 socket 文件（SocketGuard 通过 Drop）
    drop(tray_mgr);
    drop(sound_player);

    log::info!("[DEBUG] 守护进程已退出");
    Ok(())
}

/// 更新托盘图标
fn update_tray(tray_mgr: &tray::TrayManager, state: &DaemonState) {
    tray_mgr.update_state(state);
}

/// 写入状态文件（供 `claude-status status` 查询）
fn write_state_file(path: &str, state: &DaemonState) {
    let json = serde_json::json!({
        "state": state.to_string(),
        "color": state.tray_color(),
    });
    if let Err(e) = std::fs::write(path, json.to_string()) {
        log::warn!("[DEBUG] 写入状态文件失败: {}", e);
    }
}

/// 状态文件路径
fn state_file_path() -> String {
    "/tmp/claude-status.state".to_string()
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// CLI Subcommands
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// 发送 quit 命令到守护进程
fn send_quit_command() -> Result<()> {
    let socket_path = platform::socket_path();
    match ipc::send_to_daemon(&socket_path, r#"{"command":"quit"}"#) {
        Ok(()) => {
            println!("✅ 已发送退出命令到守护进程");
            Ok(())
        }
        Err(e) => {
            // 可能是守护进程未运行
            eprintln!("⚠️  无法连接到守护进程: {}", e);
            eprintln!("   守护进程可能未运行。如果确认守护进程在运行，请检查: {}", socket_path);
            Ok(())
        }
    }
}

/// 打印守护进程当前状态
fn print_status() -> Result<()> {
    let state_file = state_file_path();
    match std::fs::read_to_string(&state_file) {
        Ok(content) => {
            println!("{}", content.trim());
            Ok(())
        }
        Err(_) => {
            println!(r#"{{ "state": "守护进程未运行" }}"#);
            Ok(())
        }
    }
}

/// 持续监控模式 — 每 2 秒打印一次状态
fn watch_status() -> Result<()> {
    println!("ClaudeStatus 状态监控 (Ctrl+C 退出)...\n");
    let state_file = state_file_path();
    loop {
        match std::fs::read_to_string(&state_file) {
            Ok(content) => {
                let state: serde_json::Value =
                    serde_json::from_str(&content).unwrap_or_default();
                println!(
                    "\r[{}] 状态: {}",
                    chrono_now(),
                    state.get("state").and_then(|s| s.as_str()).unwrap_or("未知")
                );
            }
            Err(_) => {
                println!("守护进程未运行");
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// 简易时间戳（避免引入 chrono 依赖）
fn chrono_now() -> String {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => {
            let secs = dur.as_secs();
            let hours = (secs / 3600) % 24;
            let minutes = (secs / 60) % 60;
            let seconds = secs % 60;
            format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
        }
        Err(_) => "--:--:--".to_string(),
    }
}
