//! usuyo 的微信视频号获取插件（协议 §3）。
//!
//! 一条分享链接分两步取：
//! 1. 匿名：分享页自己调的 `get_feed_info`（`shortUri`），拿作者、描述、封面；不用任何账号。
//! 2. 可选：宿主传了腾讯元宝的登录 cookie 时，用它向元宝换一个临时 token，
//!    再带 token 调一次 `get_feed_info` 拿视频地址，下载 MP4。只有这一步用到元宝账号。
//!
//! 入口在 `main.rs`；这里的函数都接一份 [`Config`]，测试用它把请求指到本机的假服务。

pub mod credentials;
mod download;
pub mod error;
mod feed;
pub mod fetch;
pub mod health;
mod link;
mod yuanbao;

use std::time::Duration;

use usuyo_plugin_kit::http::{self, Client, Hosts, Policy};
use usuyo_plugin_kit::protocol::{Canonicalized, Identity, PlatformId};

pub use error::FetchError;

pub const PLATFORM: &str = "wechat-channels";
/// 清单 `sites` 里唯一的站点；元宝 cookie 只发给它。
pub const YUANBAO_SITE: &str = "yuanbao.tencent.com";

/// 普通桌面浏览器的 UA，和 ParseHub 一样不带别的指纹头。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
/// 封面和视频的 CDN 可能跳转，最多跟几次；每一跳都要在 `media_hosts` 里、是公网地址。
const MEDIA_REDIRECTS: u32 = 5;

/// 请求发到哪里。正式运行只用 [`Config::production`]；别的值只在测试里出现。
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// `https://channels.weixin.qq.com`
    pub channels: url::Url,
    /// `https://yuanbao.tencent.com`
    pub yuanbao: url::Url,
    /// 封面和视频只从这些主机下载（等于它或是它的子域）。
    pub media_hosts: Vec<String>,
    /// 下载地址（连同跳转的每一跳）只接受 https 的默认端口。
    pub media_https_only: bool,
}

/// 时长和大小上限。所有时长加起来低于清单里 `fetch.timeout`，宿主杀进程前插件已经交回元数据。
#[derive(Debug, Clone)]
pub struct Limits {
    /// 每个接口请求的总时长；canonicalize 只发一个，要在宿主的 10 s 内。
    pub api_timeout: Duration,
    pub api_bytes: u64,
    pub cover_timeout: Duration,
    pub cover_bytes: u64,
    pub video_timeout: Duration,
    pub video_bytes: u64,
    /// 这么久没收到视频的新数据就放弃这一段，交回已经拿到的部分。
    pub video_stall: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub endpoints: Endpoints,
    pub limits: Limits,
    /// 地址策略。正式运行是 [`Policy::strict`]；测试换成让本机测试服务算公网的值。
    pub policy: Policy,
}

impl Config {
    pub fn production() -> Self {
        Self {
            endpoints: Endpoints {
                channels: url::Url::parse("https://channels.weixin.qq.com").expect("constant URL"),
                yuanbao: url::Url::parse("https://yuanbao.tencent.com").expect("constant URL"),
                media_hosts: vec!["qq.com".into()],
                media_https_only: true,
            },
            limits: Limits {
                api_timeout: Duration::from_secs(8),
                api_bytes: 4 << 20,
                cover_timeout: Duration::from_secs(30),
                cover_bytes: 20 << 20,
                video_timeout: Duration::from_secs(25 * 60),
                video_bytes: 2 << 30,
                video_stall: Duration::from_secs(60),
            },
            policy: Policy::from_host(),
        }
    }
}

/// 这一条的身份。canonicalize 和 fetch 共用，只看匿名回答，不看元宝那一步：
/// 否则同一条链接登没登录元宝会得到两个身份。
///
/// `contentId` 用分享页回答里的 feed id；回答里没有时用 `sph-<短码>`。
/// 短码每次分享可能不同（设计 §3.2，推断），所以它只是退路。
pub fn identity(code: &str, feed_id: Option<&str>) -> Identity {
    Identity::Platform {
        platform: PlatformId::try_from(PLATFORM).expect("constant platform id"),
        content_id: feed_id.map_or_else(|| format!("sph-{code}"), str::to_owned),
        url: Some(link::share_url(code)),
    }
}

/// 认身份（协议 §3.5）：先离线取短码，再匿名调一次分享页的接口拿 feed id。
/// 网络不通时回 `network`，宿主照协议 §3.1 先用链接本身建条目，之后再来。
pub fn canonicalize(cfg: &Config, input: &str) -> Result<Canonicalized, FetchError> {
    let Some(code) = link::short_code(input) else {
        return Ok(Canonicalized::NoMatch);
    };
    let feed = feed::anonymous(cfg, &code)?;
    Ok(Canonicalized::Match {
        identity: identity(&code, feed.id.as_deref()),
        resolved_url: None,
    })
}

/// 只连 `base` 这一台主机的客户端。接口的请求都不跟重定向，免得 cookie 头跟着跳到别的主机。
fn api_client(cfg: &Config, base: &url::Url) -> Client {
    let host = base.host_str().unwrap_or_default();
    Client::new(USER_AGENT, Hosts::only([host])).with_policy(cfg.policy)
}

/// 下载封面和视频用的客户端：只连 `media_hosts` 和它们的子域，请求里不带任何凭据。
fn media_client(cfg: &Config) -> Client {
    let hosts = cfg
        .endpoints
        .media_hosts
        .iter()
        .flat_map(|h| [h.clone(), format!(".{h}")]);
    Client::new(USER_AGENT, Hosts::only(hosts)).with_policy(cfg.policy)
}

/// 读完一个接口回答，超过上限算接口改了。
fn read_body(response: &mut http::Response, limit: u64) -> Result<Vec<u8>, FetchError> {
    response.read_to_vec(limit).map_err(|e| match e {
        http::Error::TooLarge(_) => {
            FetchError::UpstreamChanged(format!("response is larger than {limit} bytes"))
        }
        other => FetchError::Network(other.to_string()),
    })
}

fn retry_after(response: &http::Response) -> Option<u64> {
    response.header("retry-after")?.trim().parse().ok()
}

/// 给用户看的一小段平台文字：去掉标签、控制字符，截短。
fn plain_snippet(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if in_tag || c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim().chars().take(120).collect()
}
