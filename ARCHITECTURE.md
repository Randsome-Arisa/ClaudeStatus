# ARCHITECTURE: ClaudeStatus

ClaudeStatus 的架构决策、设计权衡和替代方案记录。这篇文章回答 "为什么这么做" 而非 "怎么做" —— 后者见 README.md 和源代码注释。

## 目录

- [架构模式：Daemon + IPC](#架构模式daemon--ipc)
- [状态机设计](#状态机设计)
- [并发模型：同步线程 + Channel](#并发模型同步线程--channel)
- [资源内嵌策略](#资源内嵌策略)
- [错误处理策略](#错误处理策略)
- [Linux 托盘：GTK 事件循环的必要性](#linux-托盘gtk-事件循环的必要性)
- [通知频率限制](#通知频率限制)
- [事件合批优化](#事件合批优化)
- [超时退化机制](#超时退化机制)
- [平台抽象](#平台抽象)

---

## 架构模式：Daemon + IPC

```
┌─────────────────────────┐       JSON via Unix Socket
│  Claude Code            │ ──────────────────────────────┐
│  Hooks (5 events)       │                               │
│  echo '{"event":"..."}' │                               ▼
│  | nc -U /tmp/...sock   │              ┌────────────────────────────┐
└─────────────────────────┘              │  ClaudeStatus Daemon       │
                                         │                            │
                                         │  IPC Listener (thread)     │
                                         │       │ mpsc::channel      │
                                         │       ▼                    │
                                         │  主线程 (Event Loop)       │
                                         │   ├─ 状态机 (state.rs)     │
                                         │   ├─ 系统托盘 (tray.rs)    │
                                         │   ├─ 音频播放 (sound.rs)   │
                                         │   └─ 桌面通知 (notify.rs)  │
                                         └────────────────────────────┘
```

### 为什么不用单体架构

**被拒绝的方案 A**: 每个 Hook 事件直接调用 shell 命令发送通知和播放声音。

| 维度 | 方案 A (Shell Only) | 方案 B (Monolithic Binary) | 方案 C (Daemon+IPC) |
|------|---------------------|---------------------------|---------------------|
| 系统托盘 | ❌ 不支持 | ✅ 支持 | ✅ 支持 |
| 状态管理 | ❌ 无状态 | ⚠️ 需持久化 | ✅ 内存状态机 |
| Hook 性能 | ✅ 零开销 | ❌ 每次启动进程 | ✅ 极简 echo + nc |
| 跨平台维护 | ❌ 两套代码 | ✅ 单套 Rust | ✅ 单套 Rust |
| 扩展性 | ❌ 差 | ⚠️ 单体瓶颈 | ✅ IPC 解耦 |
| 调试复杂度 | ✅ 简单 | ⚠️ 中等 | ⚠️ 中等（多进程） |

**选择方案 C 的核心原因**:

1. **Hook 脚本极简**。一行 `echo` + `nc`，1 秒超时，守护进程未运行时静默失败 —— 不影响 Claude Code 正常工作。这是最重要的设计约束：Hook 脚本的执行开销直接加到每次工具调用的延迟上。

2. **状态管理在内存中**。守护进程常驻后台，状态机在内存中运行，无需从文件或数据库读取状态。状态变化时同步写入 `/tmp/claude-status.state` 供 CLI 查询。

3. **解耦利于扩展**。IPC 通道是唯一的外部接口。未来添加手机推送、Telegram Bot、多 session 跟踪等功能时，只需在守护进程中添加新的输出模块，Hook 配置和 IPC 协议完全不变。

### 为什么 IPC 用 Unix Socket 而非 HTTP

- **零网络开销**。Unix Domain Socket 在内核态直接传递数据，无 TCP 握手。
- **权限天然隔离**。Socket 文件在 `/tmp/` 下，只有同一用户可连接（文件权限 0o755）。
- **协议极简**。单行 JSON + `\n` 分隔，无需 HTTP header、method、status code 等元数据。
- **nc 一行搞定**。`echo '{"event":"done"}' | nc -w 1 -U /tmp/claude-status.sock`，无需 curl 或其他 HTTP 客户端。

---

## 状态机设计

### 设计原则

1. **编译期穷举检查**。`DaemonState` 是 Rust `enum`，`transition()` 使用 `match (current, event)` 穷举所有状态-事件组合。编译器保证不会遗漏转换。

2. **纯函数**。`transition(&DaemonState, &HookEvent) -> DaemonState` 无副作用，可独立测试。输出模块（托盘/声音/通知）由主事件循环在状态变化后驱动。

3. **容错导向**。非法 JSON、未知事件、空消息 → 保持当前状态不变。ERROR 状态在 30 秒后自动恢复到 IDLE。

### 转换规则

```
                          ┌─────────┐
      UserPromptSubmit ──►│  IDLE   │◄──────────── 60s timeout
         / PreToolUse     └────┬────┘
                              │
                              ▼
     PostToolUse / ┌─────┐  ┌─────────┐    Stop    ┌─────────┐
     PreToolUse ──►│WAIT │◄─│ WORKING ├───────────►│  DONE   │
                   │ ING │  └─────────┘            └─────────┘
                   └─────┘
                       ▲
                       │  PermissionRequest
                       │
          ┌──────── ERROR ──── 30s timeout ──┐
```

### 为什么 DONE→IDLE 是 60 秒而不是永久

DONE 状态表示 Claude 刚完成一个任务。当用户提交新 prompt（`UserPromptSubmit` 触发）或 Claude 调用新工具（`PreToolUse` 触发）时，直接切换到 WORKING 而不经过 IDLE，托盘颜色从红直接变绿，给用户 "连续工作" 的视觉暗示。60 秒无事件后恢复到 IDLE（灰色），表示 "当前无活动 session"。

### 为什么 WAITING 没有超时退化

WAITING 状态的重置取决于用户行为（点击授权或忽略）。用户可能在读代码、思考，或者离开座位。对 WAITING 设置超时会错误地将"用户正在阅读"误判为"空闲"。WAITING 只能由用户操作（`resumed` 事件）或 session 结束（`done` 事件）来退出。

---

## 并发模型：同步线程 + Channel

### 为什么不用 tokio

ClaudeStatus 的事件量极低 —— 每次 Claude Code 调用工具才产生 1 条 IPC 消息，典型频率是每分钟 3-10 次。对于这种负载，`tokio` 的异步运行时完全过度：

- **tokio runtime 本身 ~2MB**，接近整个 binary 的大小
- **异步代码的认知开销**。`std::thread` + `mpsc::channel` 对于新手 Rust 开发者（本项目的学习目标）更直观
- **IPC listener 是阻塞 I/O**。`BufReader::lines()` 在独立线程中阻塞读取，不占用主线程

### 线程模型

```
主线程 (main)
  │
  ├─[spawn]──► IPC Listener 线程
  │              │ 阻塞等待 Unix Socket 连接
  │              │ 逐行解析 JSON
  │              │ tx.send(msg) → mpsc channel
  │              ▼
  │          [channel] ──► rx.recv_timeout(1s)
  │                           │
  ▼                           ▼
  事件循环 ◄──────────  IpcMessage::Event / Quit
  │
  ├─ 状态转换 (state::transition)
  ├─ 更新托盘 (tray.update_state)
  ├─ 播放声音 (sound.play)
  ├─ 发送通知 (notify.notify)
  └─ 写入状态文件
```

`mpsc::channel` 天然保证串行化：所有 IPC 事件按到达顺序处理，状态机不会被并发修改。

---

## 资源内嵌策略

### 为什么用 `include_bytes!` 而不是文件系统

```
Binary size impact:
  done.wav    229 KB
  wating.wav  320 KB
  idle.png     ~1 KB
  working.png  ~1 KB
  done.png     ~1 KB
  waiting.png  ~1 KB
  ─────────────────
  Total added  ~554 KB (约 15% of binary)
```

**权衡**: 二进制增大 ~554KB，换取零外部依赖、零文件路径问题、零安装步骤。用户下载一个文件即可运行 —— 不需要解压、不需要配置路径、不需要担心文件丢失。

### WAV 格式的选择

rodio 支持 WAV、MP3、OGG、FLAC。选择 WAV 因为:
- **无解码器依赖**。WAV 是未压缩 PCM，rodio 解码 WAV 不需要额外 C 库。
- **CC0 音效广泛可用**。freesound.org 上有大量 CC0 WAV 音效。
- **文件小**。两个 WAV 合计 549KB，对 binary 体积影响可接受。

### 圆形 PNG 图标的生成

不使用运行时程序化生成的纯色方块，而是预先在 `assets/` 目录放置 32×32 RGBA PNG:
- **抗锯齿圆形**。1px 半透明边缘过渡，视觉效果平滑。
- **运行时代价零**。`png::Decoder` 只在守护进程启动时解码一次，预加载 4 个 `Icon` 后直接复用。
- **易于替换**。用户如果想自定义图标，替换 `assets/*.png` 后重新编译即可。

---

## 错误处理策略

### anyhow 全项目统一

所有公开函数返回 `anyhow::Result<T>`。原则：
- **库代码用 `Context` 附加语义**。`.with_context(|| "无法创建 IPC listener")` 而非裸 `?`。
- **main() 直接返回 `Result`**。错误自动打印，无需手动处理。
- **错误信息面向用户**。不包含 Rust 内部类型名或堆栈信息（除非 verbose 模式）。

### 音频播放的容错降级

音频是 "最好有" 而非 "必须有" 的功能。`sound.rs` 使用两层保护：

```rust
// 外层：catch_unwind 防止 panic 传播
let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
    self.play_inner(event)
}));
// 内层：Result 错误只记录日志，不返回给调用者
```

如果音频后端初始化失败（无 PulseAudio/PipeWire），守护进程继续运行，托盘和通知正常工作。用户只会在日志中看到一条 warning。

### IPC 连接失败的处理

Hook 脚本使用 `nc -w 1`（1 秒超时）+ `|| true`（静默失败）：
```bash
echo '{"event":"working"}' | nc -w 1 -U /tmp/claude-status.sock || true
```

守护进程未运行时，`nc` 在 1 秒后超时退出，返回码被 `|| true` 吞掉，Claude Code 工具调用不受任何影响。

---

## Linux 托盘：GTK 事件循环的必要性

### 问题

`tray-icon` 在 Linux 上使用 `libayatana-appindicator3`，它通过 D-Bus 的 StatusNotifierItem 协议与 GNOME Shell 通信。在 `app_indicator_new()` 中，D-Bus 注册是同步的 —— 但注册后，GNOME Shell（通过 `ubuntu-appindicators` 扩展）会向应用发出 D-Bus 属性查询（获取图标、状态、菜单等）。**这些查询回调需要通过 GLib 主循环来处理。**

如果只调用 `gtk::init()` 但从不迭代 GTK 事件循环（`gtk::main_iteration_do()`），Shell 的属性查询永远不会被响应，托盘图标永远不会显示。

### 解决方案

在守护进程的主事件循环中，每次 IPC 事件处理后，额外处理所有挂起的 GTK 事件：

```rust
#[cfg(target_os = "linux")]
{
    while gtk::events_pending() {
        gtk::main_iteration_do(false);  // false = 非阻塞
    }
}
```

守护进程启动后立刻迭代 10 次（每次间隔 50ms）确保初始 D-Bus 注册完成。

### 为什么不用 GtkApplication

`GtkApplication` 会自动管理 GTK 事件循环，但它要求整个应用围绕 GTK 的主循环构建。对于 ClaudeStatus，核心事件循环是 IPC channel 驱动的，GTK 事件处理是辅助性的。混用会导致控制流复杂化。

---

## 通知频率限制

### 为什么限制 10 秒

快速状态切换（WORKING→DONE→WORKING→DONE）在连续调用多个工具时很常见。如果每次状态切换都发送桌面通知，用户会在几秒内收到 3-5 条通知 —— 这是通知轰炸。

10 秒的频率限制确保了：
- 用户不会因为通知太多而关闭通知（defensive design）
- 快速切换时只有第一条通知会显示
- "等待授权" 这类需要用户 action 的通知仍然会正常显示（10 秒间隔足够）

### 实现

```rust
// notify.rs
if let Some(last) = self.last_notification {
    if now.duration_since(last).as_secs() < 10 {
        return;  // 跳过，不记录为 "已发送"
    }
}
```

注意：被跳过的通知不更新 `last_notification` 时间戳 —— 这确保了上次发送 10 秒后，下一条通知一定会发出。

---

## 事件合批优化

### 问题：频繁状态切换的托盘延迟

`tray.set_icon()` 在 Linux 上通过 `libayatana-appindicator3` → GDBus 同步调用更新 GNOME Shell 面板图标。每次调用阻塞当前线程 5~50ms，等待 D-Bus daemon + GNOME Shell 往返。

在频繁 waiting↔working 切换场景中（Claude 快速连续触发 PermissionRequest + PostToolUse），事件循环按以下串行流程处理：

```
recv_timeout → set_icon(yellow) → D-Bus 等待 → GTK pump
            → recv_timeout → set_icon(green)  → D-Bus 等待 → GTK pump
```

两次 D-Bus 同步调用累积产生可感知的延迟。

### 解决方案：事件合批（Event Coalescing）

第一个事件到达后，非阻塞排空 channel 中所有后续消息。**每个事件都通过状态机处理**（保证连续多步转换正确，如 WORKING→WAITING→WORKING），但只在最后执行**一次** UI 更新。

```
recv_timeout → 取出事件1 (waiting) → transition: WORKING → WAITING
             → try_recv: 事件2 (resumed) → transition: WAITING → WORKING
             → try_recv: 队列空 → break
             → 单次 set_icon(green) + D-Bus 往返 → GTK pump
```

效果：中间状态被跳过，D-Bus 调用次数从 N 次降到 1 次。

### 设计权衡

| 维度 | 合批前 | 合批后 |
|------|--------|--------|
| D-Bus 调用数 | 每事件 1 次 | 每批次 1 次 |
| 中间状态可见性 | 每个状态都渲染 | 只渲染最终状态 |
| 中间声音/通知 | 每个状态触发 | **被跳过**（有意为之） |
| 状态转换正确性 | 每次单步 | 等效（连续多步 transition） |

**跳过中间声音/通知是预期行为**：如果用户在 100ms 内完成 PermissionRequest → 批准 → PostToolUse，中间 Waiting 状态的提示音和通知对用户没有实际价值（用户刚操作完），反而形成噪音。

注意：合批只在 channel 中有**已排队**的事件时生效。正常单事件场景（事件间隔 > 1s）行为不变。

---

## 超时退化机制

状态机有两个定时转换，在主事件循环的 timeout 分支中处理：

| 状态 | 超时 | 目标 | 含义 |
|------|------|------|------|
| DONE | 60s | IDLE | Claude 完成任务后长时间无新任务 → 恢复到空闲 |
| ERROR | 30s | IDLE | 短暂错误后自动恢复（比 DONE 更快，因为错误是异常的） |

超时检查使用 `Instant::now() - state_since`，精度由 `recv_timeout(1s)` 控制（实际超时可能在 60-61 秒之间）。

---

## 平台抽象

`platform.rs` 集中管理 Linux/Windows 差异：

| 维度 | Linux | Windows |
|------|-------|---------|
| IPC 传输 | Unix Domain Socket (`/tmp/claude-status.sock`) | Named Pipe (`\\.\pipe\claude-status`) |
| 托盘后端 | `libayatana-appindicator3` (GTK3) | Win32 `Shell_NotifyIcon` |
| 通知后端 | libnotify (D-Bus) | WinRT ToastNotification |
| 音频后端 | PulseAudio / PipeWire | WASAPI |
| Hook 命令 | `nc -U` | `claude-status-send.exe` |
| 事件泵 | `gtk::main_iteration_do()` | `PeekMessageW`/`DispatchMessageW` |
| 系统依赖 | `gtk = "0.18"` | `windows-sys = "0.59"` |

平台特定代码通过 `#[cfg(target_os = "...")]` 条件编译，而非运行时检测。编译后的 binary 只包含目标平台的代码路径。

---

## Windows: claude-status-send 与 Named Pipe

### 为什么需要独立的 send 二进制

Windows 没有 `nc`（netcat）这类通用工具可以直接写入 Named Pipe。而 PowerShell 变通方案存在严重问题：

```powershell
# 不可行的 PowerShell 方案（过重、启动慢、字符转义复杂）
powershell -Command "$p = ...; $p.Connect(1000); ..."
```

- **启动开销大**。PowerShell 冷启动 ~1-2 秒，每次 Hook 触发都会延迟 Claude Code 工具调用。
- **JSON 引号转义地狱**。嵌套的引号在 PowerShell 命令行中极难正确转义。
- **依赖不确定**。不同 Windows 版本 / 配置的 PowerShell 版本不同。

因此需要一个专用的微型 helper：`claude-status-send.exe`（~460KB）。

### 设计

```
claude-status-send.exe --event working
  │
  ├─ clap 解析参数
  ├─ 手工构造 JSON 字符串（无 serde_json 依赖，缩小二进制）
  ├─ interprocess::Stream::connect("\\.\pipe\claude-status")
  ├─ write_all + flush
  └─ 退出（返回码 0 = 成功, 非 0 = 失败）
```

依赖只有 3 个 crate：`clap`、`interprocess`、`anyhow`。编译后仅 460KB，启动时间 < 50ms。

### Windows 窗口消息泵

与 Linux GTK 事件循环同理，Windows 上 `tray-icon` 通过 `Shell_NotifyIconW(NIM_ADD)` 注册图标后，用户的右键点击、Taskbar 重启等事件通过 `WM_USER_TRAYICON` 消息发送到隐藏窗口。这些消息需要 `PeekMessageW` + `DispatchMessageW` 来分发：

```rust
#[cfg(target_os = "windows")]
{
    let mut msg = unsafe { std::mem::zeroed() };
    unsafe {
        while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
```

`PM_REMOVE` 确保消息被取出后立即分发到对应的 WindowProc（tray-icon 内部注册的窗口过程）。

---

## 关键 Crate 选型

| Crate | 版本 | 选型理由 |
|-------|------|---------|
| `tray-icon` | 0.19 | 纯 Rust 托盘 API，Linux (libappindicator) + Windows (Win32) 统一接口 |
| `interprocess` | 2 | 跨平台本地 socket，`GenericFilePath` 处理 Linux/Win 路径差异 |
| `rodio` | 0.20 | 纯 Rust 音频，无需系统级解码器 |
| `notify-rust` | 4 | 跨平台桌面通知，单一 API |
| `clap` | 4 (derive) | 声明式 CLI，自动 `--help` 生成 |
| `anyhow` | 1 | 全项目统一错误处理，适合应用层（非库） |
| `png` | 0.17 | 编译期内嵌圆形图标 → 运行时解码 |

---

## 已知限制

1. **单 Session**。v0.1 只跟踪全局状态。多个 Claude Code session 同时运行时，托盘显示 "最紧急" 状态，不区分 session。
2. **无 macOS 支持**。无测试设备，macOS 需等待社区贡献或后续版本。
3. **无安装脚本**。当前安装方式为手动下载 binary + `./claude-status install`。GitHub Release 发布后需要 curl | bash 一键安装脚本。
4. **GNOME Shell 扩展依赖**。Wayland + GNOME 45+ 需要 `appindicator` 扩展。Ubuntu 24.04 预装，其他发行版可能需要手动安装。
5. **托盘图标仅在图形会话中可用**。纯 SSH 会话、无 D-Bus 的服务器环境无法使用托盘功能。
