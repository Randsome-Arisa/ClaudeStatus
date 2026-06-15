// Hook 安装/卸载
//
// 自动修改 ~/.claude/settings.json（Linux）或
// %USERPROFILE%\.claude\settings.json（Windows），注入或移除 Claude Code hook 配置。
// Hook 格式遵循 Claude Code 规范：{matcher, hooks: [{command, type}]}
//
// 平台差异：
//   Linux:   echo '{"event":"..."}' | nc -w 1 -U /tmp/claude-status.sock || true
//   Windows: claude-status-send.exe --event ...

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// 返回平台特定的 hook 命令映射
///
/// Linux 使用 `nc`（netcat）写入 Unix Socket，
/// Windows 使用 `claude-status-send.exe` 写入 Named Pipe。
fn hook_configs() -> Vec<(&'static str, String)> {
    #[cfg(target_os = "windows")]
    {
        vec![
            // UserPromptSubmit 在用户提交 prompt 时立即触发，
            // 比 PreToolUse 更早，确保 thinking 阶段图标就变绿。
            ("UserPromptSubmit",  r#"claude-status-send.exe --event working"#.into()),
            ("PreToolUse",        r#"claude-status-send.exe --event working"#.into()),
            ("PostToolUse",       r#"claude-status-send.exe --event resumed"#.into()),
            ("Stop",              r#"claude-status-send.exe --event done"#.into()),
            ("PermissionRequest", r#"claude-status-send.exe --event waiting"#.into()),
        ]
    }
    #[cfg(not(target_os = "windows"))]
    {
        vec![
            // UserPromptSubmit 在用户提交 prompt 时立即触发，
            // 比 PreToolUse 更早，确保 thinking 阶段图标就变绿。
            ("UserPromptSubmit",  r#"echo '{"event":"working"}' | nc -w 1 -U /tmp/claude-status.sock || true"#.into()),
            ("PreToolUse",        r#"echo '{"event":"working"}' | nc -w 1 -U /tmp/claude-status.sock || true"#.into()),
            ("PostToolUse",       r#"echo '{"event":"resumed"}' | nc -w 1 -U /tmp/claude-status.sock || true"#.into()),
            ("Stop",              r#"echo '{"event":"done"}' | nc -w 1 -U /tmp/claude-status.sock || true"#.into()),
            ("PermissionRequest", r#"echo '{"event":"waiting"}' | nc -w 1 -U /tmp/claude-status.sock || true"#.into()),
        ]
    }
}

/// 安装 hooks 到 settings.json
pub fn install_hooks() -> Result<()> {
    let settings_path = get_settings_path()?;

    let mut settings: Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path)
            .with_context(|| "无法读取 settings.json")?;
        serde_json::from_str(&content).unwrap_or_else(|e| {
            log::warn!("[DEBUG] settings.json 解析失败，使用空配置: {}", e);
            Value::Object(Map::new())
        })
    } else {
        Value::Object(Map::new())
    };

    let hooks = settings
        .as_object_mut()
        .context("settings.json 不是有效的 JSON 对象")?
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()));

    let hooks_obj = hooks
        .as_object_mut()
        .context("hooks 字段不是有效的 JSON 对象")?;

    for (hook_name, command) in &hook_configs() {
        let entry = hooks_obj
            .entry(hook_name.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));

        let arr = entry
            .as_array_mut()
            .context("hook 条目不是有效数组")?;

        // 查找已有的 matcher="" 组（匹配所有工具调用）
        let existing = arr.iter_mut().find(|v| {
            v.get("matcher")
                .and_then(|m| m.as_str())
                .map(|m| m.is_empty())
                .unwrap_or(false)
        });

        if let Some(group) = existing {
            // 追加到已有 group 的 hooks 数组
            let group_hooks = group
                .as_object_mut()
                .and_then(|g| g.get_mut("hooks"))
                .and_then(|h| h.as_array_mut());

            if let Some(group_hooks_arr) = group_hooks {
                if !command_already_installed(group_hooks_arr) {
                    group_hooks_arr.push(make_hook_entry(command));
                    log::info!("[DEBUG] 已追加到现有 {} matcher", hook_name);
                }
            }
        } else {
            // 创建新的 matcher group
            let new_group = serde_json::json!({
                "matcher": "",
                "hooks": [make_hook_entry(command)]
            });
            arr.push(new_group);
            log::info!("[DEBUG] 已创建新的 {} matcher group", hook_name);
        }
    }

    let new_content = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, new_content)
        .with_context(|| format!("无法写入 {}", settings_path.display()))?;

    println!("✅ Claude Code hooks 已安装到 {}", settings_path.display());
    println!("   请重启 Claude Code session 使新 hook 配置生效。");
    Ok(())
}

/// 创建单个 hook 命令条目
fn make_hook_entry(command: &str) -> Value {
    serde_json::json!({
        "type": "command",
        "command": command,
    })
}

/// 检查命令是否已安装
fn command_already_installed(hooks: &[Value]) -> bool {
    hooks.iter().any(|h| {
        h.get("command")
            .and_then(|c| c.as_str())
            .map(|c| c.contains("claude-status"))
            .unwrap_or(false)
    })
}

/// 移除 hooks 配置
pub fn uninstall_hooks() -> Result<()> {
    let settings_path = get_settings_path()?;

    if !settings_path.exists() {
        println!("⚠️  settings.json 不存在，无需卸载。");
        return Ok(());
    }

    let content = std::fs::read_to_string(&settings_path)
        .with_context(|| "无法读取 settings.json")?;
    let mut settings: Value = serde_json::from_str(&content)
        .with_context(|| "settings.json 解析失败")?;

    let mut removed = 0;

    if let Some(hooks) = settings.get_mut("hooks") {
        if let Some(hooks_obj) = hooks.as_object_mut() {
            for (hook_name, _) in &hook_configs() {
                if let Some(arr) = hooks_obj.get_mut(*hook_name) {
                    if let Some(groups) = arr.as_array_mut() {
                        // 遍历所有 matcher group
                        for group in groups.iter_mut() {
                            if let Some(group_obj) = group.as_object_mut() {
                                if let Some(group_hooks) = group_obj.get_mut("hooks") {
                                    if let Some(hook_arr) = group_hooks.as_array_mut() {
                                        let before = hook_arr.len();
                                        hook_arr.retain(|h| {
                                            !h.get("command")
                                                .and_then(|c| c.as_str())
                                                .map(|c| c.contains("claude-status"))
                                                .unwrap_or(false)
                                        });
                                        removed += before - hook_arr.len();
                                    }
                                }
                            }
                        }
                        // 移除空的 matcher group
                        groups.retain(|g| {
                            g.get("hooks")
                                .and_then(|h| h.as_array())
                                .map(|a| !a.is_empty())
                                .unwrap_or(false)
                        });
                        // 如果整个 hook 类型为空，移除
                        if groups.is_empty() {
                            hooks_obj.remove(*hook_name);
                        }
                    }
                }
            }
        }
    }

    let new_content = serde_json::to_string_pretty(&settings)?;
    std::fs::write(&settings_path, new_content)
        .with_context(|| format!("无法写入 {}", settings_path.display()))?;

    println!("✅ 已从 {} 移除 {} 个 hook 配置", settings_path.display(), removed);
    Ok(())
}

/// 获取 settings.json 路径
fn get_settings_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("无法确定 HOME 目录")?;
    Ok(home.join(".claude").join("settings.json"))
}
