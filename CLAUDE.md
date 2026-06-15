# ClaudeStatus 项目上下文

## 项目简介

Claude Code 桌面状态提示工具 — 系统托盘图标 + 声音 + 桌面通知，实时反映 Claude 工作状态，无需反复切回终端。

**当前版本**: v0.1.0 | **平台**: Linux (Ubuntu 24.04) + Windows 10/11

## 规划文档

所有设计决策和测试计划位于 gstack 项目目录：

- **设计文档**: `~/.gstack/projects/ClaudeStatus/arisa-unknown-design-20260602-181244.md`
  - 产品设计、架构选择（Approach C: Daemon+IPC）、状态机定义、CLI 规格、风险评估
- **测试计划**: `~/.gstack/projects/ClaudeStatus/arisa-unknown-eng-review-test-plan-20260602-181500.md`
  - 状态机 9 个测试、IPC 4 个测试、install 3 个测试、集成测试 4 个
- **架构说明**: `ARCHITECTURE.md`（本仓库）
  - 设计决策、技术权衡、替代方案记录

每次新 session 开始时，先阅读上述文件了解完整上下文。

## 项目文件

```
~/桌面/ClaudeStatus/
├── rust-learning-guide.md          # Rust 知识手册（参考用）
├── tray-demo/                      # 概念验证（已完成，保留存档）
└── claude-status/                  # 正式项目 ← 工作目录
    ├── README.md                   # 用户文档
    ├── ARCHITECTURE.md             # 架构设计与技术决策
    ├── Cargo.toml                  # workspace 根（2 个成员）
    ├── claude-status/              # 守护进程（主程序）
    │   ├── Cargo.toml              # 14 个依赖
    │   └── src/
    │       ├── main.rs             # CLI + daemon 事件循环（含平台事件泵和事件合批, 6 tests）
    │       ├── state.rs            # 状态机 (21 tests)
    │       ├── ipc.rs              # IPC listener (Unix Socket / Named Pipe, 10 tests)
    │       ├── tray.rs             # 系统托盘（tray-icon, 4 个圆形 PNG 内嵌）
    │       ├── sound.rs            # 音频 (rodio + include_bytes! WAV)
    │       ├── notify.rs           # 桌面通知 (notify-rust, 10s 限频)
    │       ├── install.rs          # Hook 安装/卸载（平台自适应：Linux nc / Win send）
    │       ├── config.rs           # 日志 (env_logger)
    │       └── platform.rs         # Linux/Windows 条件编译
    ├── claude-status-send/         # IPC 发送器（小型 helper, ~460KB）
    │   ├── Cargo.toml              # 3 个依赖
    │   └── src/
    │       └── main.rs             # --event / --command → IPC 写入
    ├── assets/
    │   ├── idle.png                # 空闲图标（灰色圆形, 32×32）
    │   ├── working.png             # 工作中图标（绿色圆形, 32×32）
    │   ├── done.png                # 已完成图标（红色圆形, 32×32）
    │   ├── waiting.png             # 等待授权图标（黄色圆形, 32×32）
    │   ├── done.wav                # 完成提示音 (229KB)
    │   └── wating.wav              # 等待提示音 (319KB)
    └── .github/workflows/
        ├── ci.yml                  # Linux + Windows CI
        └── release.yml             # tag push → Release
```

## 架构速览

```
Claude Code hooks (UserPromptSubmit/PreToolUse/PostToolUse/Stop/PermissionRequest)
    │ echo '{"event":"..."}' | nc -U /tmp/claude-status.sock
    ▼
IPC Listener (interprocess v2) → mpsc::channel → 状态机 (state.rs)
    │
    ├── tray.rs:  托盘变色 (🟢绿🟡黄🔴红⚫灰, tray-icon + libayatana-appindicator3)
    ├── sound.rs: 播放 WAV (rodio, include_bytes!, done/wating 两音频)
    └── notify.rs: 桌面通知 (notify-rust, 10s 限频, 5s 消失)
```

**关键**: Linux 必须迭代 GTK 事件循环（`gtk::main_iteration_do(false)`），Windows 必须泵送窗口消息（`PeekMessageW` + `DispatchMessageW`），否则托盘图标无法响应 D-Bus 属性查询 / 右键菜单点击。详见 `ARCHITECTURE.md`。

## 关键技术决策

- **架构**: 守护进程 + IPC（非单体 binary），Hook 极简一行
- **并发**: 同步 `std::thread` + `mpsc::channel`（无需 tokio）
- **IPC**: `interprocess` v2, `GenericFilePath` 类型, `to_fs_name` API
- **托盘**: `tray-icon` 0.19 + `libayatana-appindicator-sys` (dlopen 加载 `libayatana-appindicator3.so`)
- **图标**: 圆形 PNG (32×32, `include_bytes!` 内嵌) → `png::Decoder` 运行时解码为 RGBA
- **音频**: `include_bytes!` 内嵌 WAV → `rodio::Decoder` 解码播放
- **错误**: `anyhow` 全项目统一，音频 `catch_unwind` 静默降级
- **Hook 格式**: `{matcher: "", hooks: [{type: "command", command: "..."}]}` — 必须嵌套
- **Hook 事件**: `UserPromptSubmit` + `PreToolUse` → working, `PostToolUse` → resumed, `Stop` → done, `PermissionRequest` → waiting
  - `UserPromptSubmit` 在用户提交 prompt 时立即触发，确保 thinking 阶段图标就变绿，比 `PreToolUse` 更早
- **GTK 事件泵**: 主循环每次迭代后 `while gtk::events_pending() { gtk::main_iteration_do(false); }`

## 已完成 (v0.1.0)

- [x] T1 项目骨架 → workspace + 全依赖（含 png 解码、windows-sys）
- [x] T2 状态机 → 5 状态 10 转换规则，21 单元测试
- [x] T3 IPC 层 → interprocess v2 (Unix Socket + Named Pipe), 10 测试
- [x] T4 守护进程生命周期 → mpsc event loop, 超时转换, quit/status
- [x] T5 CLI → clap Derive (daemon/quit/status/install/uninstall)
- [x] T6 输出模块 → tray (圆形图标 + 右击菜单) + sound + notify
- [x] T7 日志 → env_logger, --verbose/--log-file
- [x] T8 CI/CD → GitHub Actions Linux+Windows
- [x] BUGFIX 托盘图标不可见 → GTK 事件循环未迭代
- [x] ENHANCE 图标圆形化 → 4 个圆形 PNG 替代程序化方块
- [x] DOCS → ARCHITECTURE.md 架构说明文档
- [x] WINDOWS-P1 → `claude-status-send` helper binary (460KB)
- [x] WINDOWS-P2 → `install.rs` 平台自适应 Hook 命令
- [x] WINDOWS-P3 → Windows 消息泵 (`PeekMessageW`/`DispatchMessageW`)
- [x] WINDOWS-P4 → 交叉编译通过 (`x86_64-pc-windows-gnu`)
- [x] DOCS → README.md + CLAUDE.md Windows 安装/使用文档
- [x] BUGFIX 事件合批丢弃中间状态声音/通知 → `record_side_effects()` 补触发机制 (6 tests)

**用户环境:**
- ✅ Ubuntu 24.04, GNOME 46, Wayland
- ✅ `ubuntu-appindicators@ubuntu.com` 扩展已启用
- ✅ `libayatana-appindicator3-1` 已安装
- ✅ `~/.claude/settings.json` hooks 已配置
- ✅ 通知和音频已验证正常
- ✅ 圆形托盘图标正常显示

**待完成:**
- [ ] 集成测试 (`tests/integration_test.rs`) — 4 个端到端测试
- [ ] Windows 物理机实际验证（托盘、通知、音频）
- [ ] GitHub Release 首次发布（4 个二进制: Linux × 2 + Win × 2）
- [ ] 安装脚本 (curl | bash 用于 Linux, PowerShell 用于 Windows)

## 常用命令

```bash
# 开发
cargo check          # 快速编译检查
cargo test           # 运行 27 个单元测试
cargo build --release  # Release 编译 (~3.3MB)

# 使用
./claude-status install          # 安装 hooks
./claude-status daemon --verbose &  # 启动守护进程
./claude-status status           # 查看当前状态 (JSON)
./claude-status quit             # 停止守护进程

# 手动测试
echo '{"event":"working"}'  | nc -w 1 -U /tmp/claude-status.sock
echo '{"event":"waiting"}'  | nc -w 1 -U /tmp/claude-status.sock
echo '{"event":"done"}'     | nc -w 1 -U /tmp/claude-status.sock
echo '{"command":"quit"}'   | nc -w 1 -U /tmp/claude-status.sock
```

## 系统依赖

**编译时需要**: `libasound2-dev libdbus-1-dev libnotify-dev libgtk-4-dev libayatana-appindicator3-dev`

**运行时**: Ubuntu 24.04 桌面版已预装全部运行时库（D-Bus, libnotify, GTK3, libayatana-appindicator3, PipeWire/PulseAudio），无需额外安装。
