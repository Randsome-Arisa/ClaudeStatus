// 音频播放
//
// 使用 rodio crate 播放内嵌的 WAV 音效文件。
// 编译时通过 include_bytes! 将 assets/ 下的音频嵌入二进制。
// 音频播放失败时静默降级，不影响其他功能。

use anyhow::Result;
use rodio::{OutputStream, Sink};
use std::io::Cursor;

/// 编译时内嵌音效文件
const SOUND_DONE: &[u8] = include_bytes!("../../assets/done.wav");
const SOUND_WAITING: &[u8] = include_bytes!("../../assets/wating.wav");

/// 音效类型
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundEvent {
    /// 任务完成
    Done,
    /// 等待授权
    Waiting,
}

/// 音频播放器
pub struct SoundPlayer {
    _stream: OutputStream,
    stream_handle: rodio::OutputStreamHandle,
}

impl SoundPlayer {
    /// 创建音频播放器（初始化音频后端）
    pub fn new() -> Result<Self> {
        let (stream, stream_handle) =
            OutputStream::try_default().map_err(|e| {
                anyhow::anyhow!("音频后端初始化失败: {}", e)
            })?;
        log::info!("[DEBUG] 音频播放器已初始化（done.wav + wating.wav 已内嵌）");
        Ok(SoundPlayer {
            _stream: stream,
            stream_handle,
        })
    }

    /// 播放指定音效（失败时静默降级）
    pub fn play(&self, event: SoundEvent) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.play_inner(event)
        }));

        match result {
            Ok(Ok(())) => {
                log::debug!("[DEBUG] 音效播放完成: {:?}", event);
            }
            Ok(Err(e)) => {
                log::warn!("[DEBUG] 音效播放失败: {}", e);
            }
            Err(_) => {
                log::warn!("[DEBUG] 音效播放 panic（已静默降级）");
            }
        }
    }

    fn play_inner(&self, event: SoundEvent) -> Result<()> {
        let data = match event {
            SoundEvent::Done => SOUND_DONE,
            SoundEvent::Waiting => SOUND_WAITING,
        };

        let cursor = Cursor::new(data);
        let decoder =
            rodio::Decoder::new(cursor).map_err(|e| {
                anyhow::anyhow!("WAV 解码失败: {}", e)
            })?;

        let sink = Sink::try_new(&self.stream_handle)
            .map_err(|e| anyhow::anyhow!("创建音频 sink 失败: {}", e))?;

        sink.append(decoder);
        sink.sleep_until_end();

        Ok(())
    }
}
