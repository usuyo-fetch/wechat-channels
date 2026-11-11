//! 下载封面和视频。地址来自平台的回答，不可信：只接受 [`crate::Endpoints`] 里列出的主机，
//! 有大小和时长上限，按文件头认格式。失败只返回给用户看的原因，由调用方记成缺的那一项。

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use usuyo_plugin::protocol::PluginEvent;

use crate::{Config, media_agent};

const PROGRESS_EVERY: Duration = Duration::from_secs(30);

/// 检查下载地址：协议、主机都要在名单里。
fn media_url(cfg: &Config, raw: &str) -> Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|_| "平台给的下载地址不是链接".to_owned())?;
    let scheme_ok = match url.scheme() {
        "https" => true,
        "http" => !cfg.endpoints.media_https_only,
        _ => false,
    };
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let host_ok = cfg
        .endpoints
        .media_hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")));
    if !scheme_ok {
        return Err(format!("平台给的下载地址不是 https（{}）", url.scheme()));
    }
    if !host_ok {
        return Err(format!("平台给的下载地址不在允许的主机里（{host}）"));
    }
    Ok(url)
}

/// 下载封面，按文件头认出 jpg、png、webp、gif；返回内容和扩展名。
pub fn cover(cfg: &Config, raw_url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let url = media_url(cfg, raw_url)?;
    let limit = cfg.limits.cover_bytes;
    let mut response = media_agent(cfg.limits.cover_timeout)
        .get(url.as_str())
        .header("Accept", "image/*")
        .call()
        .map_err(|e| format!("封面下载失败：{e}"))?;
    let status = response.status().as_u16();
    if !(200..=299).contains(&status) {
        return Err(format!("封面下载失败：HTTP {status}"));
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => format!("封面超过 {} MiB", limit >> 20),
            other => format!("封面下载失败：{other}"),
        })?;
    let ext = image_kind(&bytes).ok_or_else(|| "封面地址给出的不是图片".to_owned())?;
    Ok((bytes, ext))
}

fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else {
        None
    }
}

/// 下载视频到 `dest`。先写进临时目录，确认是 MP4 才移进输出目录，所以输出目录里不会留半截文件。
///
/// 下载放在另一个线程里：这边每半秒看一次进度，发 `progress` 事件；超过 `video_stall`
/// 没有新数据就放弃，让调用方交回已经拿到的元数据，而不是等宿主按无进度超时杀掉进程。
pub fn video(cfg: &Config, raw_url: &str, dest: &Path) -> Result<u64, String> {
    let url = media_url(cfg, raw_url)?;
    let limits = &cfg.limits;
    let part = tempfile::NamedTempFile::new().map_err(|e| format!("建临时文件失败：{e}"))?;
    let mut file = part
        .reopen()
        .map_err(|e| format!("打开临时文件失败：{e}"))?;
    let received = Arc::new(AtomicU64::new(0));
    let expected = Arc::new(AtomicU64::new(0));
    let (tx, rx) = mpsc::channel();
    let agent = media_agent(limits.video_timeout);
    let cap = limits.video_bytes;
    {
        let received = Arc::clone(&received);
        let expected = Arc::clone(&expected);
        std::thread::spawn(move || {
            let result = (|| -> Result<u64, String> {
                let response = agent
                    .get(url.as_str())
                    .header("Accept", "video/mp4,video/*;q=0.9,*/*;q=0.5")
                    .call()
                    .map_err(|e| format!("视频下载失败：{e}"))?;
                let status = response.status().as_u16();
                if !(200..=299).contains(&status) {
                    return Err(format!("视频下载失败：HTTP {status}"));
                }
                let length = response
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok());
                if length.is_some_and(|n| n > cap) {
                    return Err(format!("视频超过 {} MiB", cap >> 20));
                }
                expected.store(length.unwrap_or(0), Ordering::Relaxed);
                // 上限自己数：Content-Length 可能没有，也可能不实
                let mut reader = response.into_body().into_with_config().reader();
                let mut buf = vec![0u8; 256 << 10];
                let mut total = 0u64;
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(format!("视频下载中断：{e}")),
                    };
                    if total + n as u64 > cap {
                        return Err(format!("视频超过 {} MiB", cap >> 20));
                    }
                    file.write_all(&buf[..n])
                        .map_err(|e| format!("写临时文件失败：{e}"))?;
                    total += n as u64;
                    received.store(total, Ordering::Relaxed);
                }
                file.flush().map_err(|e| format!("写临时文件失败：{e}"))?;
                Ok(total)
            })();
            // 主线程可能已经放弃这一段，收不到也没关系
            tx.send(result).ok();
        });
    }

    let mut last_bytes = 0u64;
    let mut last_change = Instant::now();
    let mut last_event = 0u64;
    let mut last_event_at = Instant::now();
    let total = loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(result) => break result?,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("视频下载线程意外结束".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let now_bytes = received.load(Ordering::Relaxed);
        if now_bytes != last_bytes {
            last_bytes = now_bytes;
            last_change = Instant::now();
        } else if last_change.elapsed() > limits.video_stall {
            return Err(format!(
                "视频下载 {} 秒没有新数据，先存下其余部分",
                limits.video_stall.as_secs()
            ));
        }
        // 每 4 MiB，或有新数据时至少每 30 秒发一次：宿主 120 秒收不到 progress 就算卡住（协议 §3.4）
        if now_bytes > last_event
            && (now_bytes >= last_event + (4 << 20) || last_event_at.elapsed() >= PROGRESS_EVERY)
        {
            last_event = now_bytes;
            last_event_at = Instant::now();
            progress(now_bytes, expected.load(Ordering::Relaxed));
        }
    };

    let mut head = [0u8; 12];
    let mut check = part
        .reopen()
        .map_err(|e| format!("打开临时文件失败：{e}"))?;
    let read = check
        .read(&mut head)
        .map_err(|e| format!("读临时文件失败：{e}"))?;
    // MP4 的第一个 box 是 `ftyp`：第 4 到 8 字节
    if read < 12 || &head[4..8] != b"ftyp" {
        return Err("视频地址给出的不是能直接播放的 MP4 文件".into());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("建 media/ 失败：{e}"))?;
    }
    // 临时目录和输出目录不在同一个文件系统上时 rename 会失败，改成复制
    if let Err(e) = part.persist(dest) {
        std::fs::copy(e.file.path(), dest).map_err(|e| format!("写视频文件失败：{e}"))?;
    }
    Ok(total)
}

fn progress(done: u64, total: u64) {
    let mb = |b: u64| (b as f64 / f64::from(1 << 20) * 10.0).round() / 10.0;
    let event = PluginEvent::Progress {
        done: Some(mb(done)),
        total: (total > 0).then(|| mb(total)),
        unit: Some("MB".into()),
        message: Some("下载视频".into()),
    };
    eprintln!("{}", event.stderr_line());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_kinds() {
        assert_eq!(image_kind(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(image_kind(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(image_kind(b"<html>"), None);
    }

    #[test]
    fn media_hosts() {
        let cfg = Config::production();
        assert!(media_url(&cfg, "https://finder.video.qq.com/251/x?a=1").is_ok());
        assert!(media_url(&cfg, "https://qq.com/x").is_ok());
        assert!(media_url(&cfg, "http://finder.video.qq.com/x").is_err());
        assert!(media_url(&cfg, "https://evilqq.com/x").is_err());
        assert!(media_url(&cfg, "https://qq.com.example/x").is_err());
        assert!(media_url(&cfg, "https://127.0.0.1/x").is_err());
        assert!(media_url(&cfg, "file:///etc/passwd").is_err());
    }
}
