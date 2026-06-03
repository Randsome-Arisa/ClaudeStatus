// 日志系统配置
//
// 使用 log + env_logger，支持 --verbose 和 --log-file 选项。

use anyhow::{Context, Result};

/// 初始化日志系统
///
/// `verbose`: 是否启用详细日志（debug level）
/// `log_file`: 自定义日志文件路径（None 时默认输出到 stderr）
pub fn init_logging(verbose: bool, log_file: Option<&str>) -> Result<()> {
    let default_level = if verbose { "debug" } else { "info" };

    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default()
            .filter_or("RUST_LOG", default_level),
    );

    // 如果指定了日志文件，写入文件；否则输出到 stderr
    if let Some(path) = log_file {
        let log_path = std::path::Path::new(path);
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("无法创建日志目录: {}", parent.display()))?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .with_context(|| format!("无法打开日志文件: {}", log_path.display()))?;

        builder.target(env_logger::Target::Pipe(Box::new(file)));
        eprintln!("[ClaudeStatus] 日志写入: {}", log_path.display());
    }

    builder.init();

    Ok(())
}
