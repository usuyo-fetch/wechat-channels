//! 测试用的本机 HTTP 服务：按路由回固定内容，记下收到的每个请求。不碰真网络。

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use usuyo_fetch_wechat_channels::{Config, Endpoints, Limits};
use usuyo_plugin_kit::http::Policy;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub target: String,
    /// 名字小写。
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    /// 整个请求的原文，查哨兵值用。
    pub fn dump(&self) -> String {
        let headers: Vec<String> = self
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        format!(
            "{} {}\n{}\n\n{}",
            self.method,
            self.target,
            headers.join("\n"),
            self.body
        )
    }
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// 先发这么多字节，然后停住不再发（测下载卡住）。
    pub stall_after: Option<usize>,
    /// 回 3xx 时的 `Location`。
    pub location: Option<String>,
}

impl Reply {
    pub fn json(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into(),
            stall_after: None,
            location: None,
        }
    }
    pub fn bytes(content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: 200,
            content_type,
            body: body.into(),
            stall_after: None,
            location: None,
        }
    }
}

type Route = dyn Fn(&Request) -> Reply + Send + Sync;

pub struct Server {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub fn start(route: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let route: Arc<Route> = Arc::new(route);
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let route = Arc::clone(&route);
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let target = parts.next().unwrap_or("/").to_owned();
                    let mut headers = Vec::new();
                    loop {
                        let mut h = String::new();
                        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = h.trim_end().split_once(':') {
                            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
                        }
                    }
                    let length: usize = headers
                        .iter()
                        .find(|(k, _)| k == "content-length")
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).ok();
                    let request = Request {
                        method,
                        target,
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    };
                    let reply = route(&request);
                    log.lock().unwrap().push(request);
                    let location = reply
                        .location
                        .as_ref()
                        .map(|l| format!("Location: {l}\r\n"))
                        .unwrap_or_default();
                    let head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{location}\r\n",
                        reply.status,
                        reply.content_type,
                        reply.body.len()
                    );
                    stream.write_all(head.as_bytes()).ok();
                    match reply.stall_after {
                        Some(n) => {
                            stream.write_all(&reply.body[..n]).ok();
                            stream.flush().ok();
                            std::thread::sleep(Duration::from_secs(30));
                        }
                        None => {
                            stream.write_all(&reply.body).ok();
                        }
                    }
                });
            }
        });
        Self { base, seen }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap().clone()
    }
}

/// 测试用的地址策略：只有 127.0.0.1 算公网，不查真的 DNS。
/// `loop.test` 在 CDN 名单里，但解析到 127.0.0.2，用来测跳转去回环地址。
pub fn local_policy() -> Policy {
    fn lookup(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
        match host {
            "loop.test" => Ok(vec![SocketAddr::from(([127, 0, 0, 2], port))]),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no such host",
            )),
        }
    }
    Policy {
        lookup,
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
        ..Policy::strict()
    }
}

/// 分享页接口和 CDN 在一个服务上，元宝在另一个服务上。
pub fn config(channels: &Server, yuanbao: &Server) -> Config {
    Config {
        endpoints: Endpoints {
            channels: url::Url::parse(&channels.base).unwrap(),
            yuanbao: url::Url::parse(&yuanbao.base).unwrap(),
            media_hosts: vec!["127.0.0.1".into(), "loop.test".into()],
            media_https_only: false,
        },
        limits: Limits {
            api_timeout: Duration::from_secs(5),
            api_bytes: 1 << 20,
            cover_timeout: Duration::from_secs(5),
            cover_bytes: 1 << 20,
            video_timeout: Duration::from_secs(10),
            video_bytes: 1 << 20,
            video_stall: Duration::from_secs(1),
        },
        policy: local_policy(),
    }
}

pub fn fixture(name: &str, media: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{media}", media)
}

/// 一个能过 `ftyp` 检查的假 MP4。
pub fn mp4() -> Vec<u8> {
    let mut v = b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2".to_vec();
    v.extend(std::iter::repeat_n(0u8, 4096));
    v
}

pub const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0\0\x10JFIFfake-cover";
