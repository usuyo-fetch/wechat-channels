//! 方法 `yuanbao`：用你的腾讯元宝登录换一个临时 token，再带 token 调分享页的接口拿视频地址。
//! 账号只用在这一样上；作者、描述、封面由不带账号的 `feed-info` 取。
//!
//! 请求和回答的处理移植自 ParseHub `src/parsehub/provider_api/wxchannels.py`
//! （https://github.com/z-mio/ParseHub ，提交 4b93e2d7，2026-11-09），许可证如下：
//!
//! MIT License
//!
//! Copyright (c) 2024 梓澪
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//!
//! 和 ParseHub 的不同：不跟重定向；登录失效（401、错误码 20000）回 `login-required`，限流回 `rate-limited`，
//! 连不上回 `network`；元宝或视频号没给出视频地址（[`VideoError::Missed`]、[`VideoError::Unreadable`]）由
//! `fetch::yuanbao` 报成 `upstream-changed`，读不出的回答开头随错误交给宿主。

use serde_json::{Value, json};
use usuyo_plugin_kit::http::Cookie;

use crate::credentials::YuanbaoCookies;
use crate::feed::{self, FeedFields, Rejected, opt_str};
use crate::{
    Config, FetchError, YUANBAO_SITE, api_client, link, plain_snippet, read_body, retry_after,
};

const PARSE_API: &str = "/api/weixin/get_parse_result";
/// 元宝说登录失效时的错误码（参照源码里写死的）。
const LOGIN_EXPIRED: &str = "20000";
/// token、export id 最长多少：只挡住异常长的值。
const MAX_TOKEN: usize = 4096;

/// 这一段为什么没拿到视频。`Missed`、`Unreadable` 是平台没给出视频地址，原因是给你看的一句话；
/// `Unreadable` 另带平台回答的开头。
#[derive(Debug)]
pub enum VideoError {
    Fatal(FetchError),
    Missed(String),
    Unreadable { why: String, answer: Vec<u8> },
}

impl VideoError {
    /// 这种方法只取视频：没取到就是这次调用失败。
    pub fn into_fetch_error(self) -> FetchError {
        match self {
            VideoError::Fatal(e) => e,
            VideoError::Missed(why) => FetchError::UpstreamChanged(why),
            VideoError::Unreadable { why, answer } => {
                FetchError::UpstreamChanged(why).answered(&answer)
            }
        }
    }
}

impl From<FetchError> for VideoError {
    fn from(e: FetchError) -> Self {
        VideoError::Fatal(e)
    }
}

/// 元宝给的临时凭证。
pub struct Exchange {
    token: String,
    pub export_id: String,
    /// 带 token 调接口时当 Referer，和参照源码一样；只发给分享页的接口。
    playable_url: String,
}

/// 带 token 那次回答里取到的东西。
#[derive(Debug, Clone)]
pub struct VideoFeed {
    pub video_url: String,
    pub fields: FeedFields,
}

/// 用元宝 cookie 换临时 token。cookie 只发给 `Config.endpoints.yuanbao` 这一个地址。
pub fn exchange(
    cfg: &Config,
    code: &str,
    cookies: &YuanbaoCookies,
) -> Result<Exchange, VideoError> {
    let root = call_parse(cfg, &link::share_url(code), cookies)?;
    if let Some(error) = root.get("error").filter(|e| !e.is_null()) {
        let code = error_code(error);
        if code.as_deref() == Some(LOGIN_EXPIRED) {
            return Err(
                FetchError::LoginRequired(format!("yuanbao error code {LOGIN_EXPIRED}")).into(),
            );
        }
        return Err(VideoError::Missed(format!(
            "腾讯元宝没有给出视频地址（错误 {}）",
            code.unwrap_or_else(|| "未知".into())
        )));
    }
    if root.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(VideoError::Missed(
            "腾讯元宝没有给出视频地址，它的接口可能改了".into(),
        ));
    }
    let data = root.get("data").and_then(Value::as_object);
    let field = |key: &str| {
        data.and_then(|d| d.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let (Some(_), Some(playable)) = (field("wx_export_id"), field("playable_url")) else {
        return Err(VideoError::Missed("腾讯元宝没有给出这条视频的地址".into()));
    };
    let changed = || VideoError::Missed("腾讯元宝给的地址里没有临时凭证，它的接口可能改了".into());
    let url = url::Url::parse(playable).map_err(|_| changed())?;
    let query = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .filter(|v| {
                !v.is_empty() && v.len() <= MAX_TOKEN && v.bytes().all(|b| b.is_ascii_graphic())
            })
    };
    let (Some(token), Some(export_id)) = (query("token"), query("eid")) else {
        return Err(changed());
    };
    Ok(Exchange {
        token,
        export_id,
        playable_url: url.into(),
    })
}

/// 带 token 调分享页的接口，拿视频地址。不带任何 cookie。
pub fn video_feed(cfg: &Config, ex: &Exchange) -> Result<VideoFeed, VideoError> {
    let base = &cfg.endpoints.channels;
    let mut url = feed::api_url(cfg);
    url.query_pairs_mut()
        .append_pair("_rid", &feed::rid())
        .append_pair(
            "_pageUrl",
            &feed::page_url(cfg, "/finder-preview/pages/feed"),
        );
    let body = json!({"baseReq": {"generalToken": ex.token}, "exportId": ex.export_id});
    let mut response = api_client(cfg, base)
        .post(&url, body.to_string())
        .header("Content-Type", "application/json")
        .header("Origin", &feed::origin(base))
        .header("Referer", &ex.playable_url)
        .timeout(cfg.limits.api_timeout)
        .send()
        .map_err(|e| FetchError::Network(format!("feed request with token: {e}")))?;
    let status = response.status();
    match status {
        200..=299 => {}
        429 => {
            return Err(FetchError::RateLimited {
                host: "channels.weixin.qq.com",
                retry_after: retry_after(&response),
            }
            .into());
        }
        _ => {
            return Err(VideoError::Unreadable {
                why: format!("视频号接口带临时凭证时返回 HTTP {status}"),
                answer: read_body(&mut response, cfg.limits.api_bytes).unwrap_or_default(),
            });
        }
    }
    let raw = read_body(&mut response, cfg.limits.api_bytes)?;
    let parsed = feed::parse(&raw).map_err(|r| match r {
        Rejected::Unavailable(text) => {
            VideoError::Missed(format!("视频号接口没有给出视频：{text}"))
        }
        Rejected::Malformed(_) => VideoError::Unreadable {
            why: "视频号接口带临时凭证时的回答解析不出，接口可能改了".into(),
            answer: raw.clone(),
        },
    })?;
    let info = &parsed.feed_info;
    let nested = |key: &str| match info.get(key) {
        Some(Value::Object(m)) => opt_str(m, "videoUrl"),
        _ => Ok(None),
    };
    let found = [
        nested("h264VideoInfo"),
        opt_str(info, "videoUrl"),
        nested("h265VideoInfo"),
    ]
    .into_iter()
    .find_map(|r| r.transpose());
    let video_url = match found {
        Some(Ok(url)) => url,
        Some(Err(_)) => {
            return Err(VideoError::Missed(
                "视频号接口的视频地址字段解析不出，接口可能改了".into(),
            ));
        }
        None => {
            return Err(VideoError::Missed(
                "这一条没有视频文件（可能是图片或直播），或视频号接口没有给出视频地址".into(),
            ));
        }
    };
    Ok(VideoFeed {
        video_url,
        fields: parsed.fields,
    })
}

/// `health --probe`：发一次换 token 的请求，只看登录有没有被拒。
/// 用一条没有短码的链接，不对应任何内容；元宝对这种链接怎么回答没有实测过。
pub fn probe(cfg: &Config, cookies: &YuanbaoCookies) -> Result<bool, FetchError> {
    match call_parse(cfg, "https://weixin.qq.com/sph/", cookies) {
        Ok(root) => Ok(root
            .get("error")
            .filter(|e| !e.is_null())
            .and_then(error_code)
            .as_deref()
            != Some(LOGIN_EXPIRED)),
        Err(VideoError::Fatal(FetchError::LoginRequired(_))) => Ok(false),
        Err(VideoError::Fatal(e)) => Err(e),
        // 回答解析不出不等于登录被拒
        Err(VideoError::Missed(_) | VideoError::Unreadable { .. }) => Ok(true),
    }
}

/// 连不上时回 `Fatal(Network)`。
fn call_parse(
    cfg: &Config,
    share_url: &str,
    cookies: &YuanbaoCookies,
) -> Result<Value, VideoError> {
    let base = &cfg.endpoints.yuanbao;
    let mut url = base.clone();
    url.set_path(PARSE_API);
    let mut referer = base.clone();
    referer.set_path("/");
    // cookie 只给元宝这一台主机；不跟重定向
    let cookie = Cookie::new(cookies.header(), [base.host_str().unwrap_or_default()]);
    let body = json!({"type": "video_channel_url", "url": share_url, "scene": 1});
    let mut response = api_client(cfg, base)
        .post(&url, body.to_string())
        .header("Content-Type", "application/json")
        .header("Origin", &feed::origin(base))
        .header("Referer", referer.as_str())
        .cookie(&cookie)
        .timeout(cfg.limits.api_timeout)
        .send()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = response.status();
    match status {
        200..=299 => {}
        401 => return Err(FetchError::LoginRequired("yuanbao HTTP 401".into()).into()),
        429 => {
            return Err(FetchError::RateLimited {
                host: YUANBAO_SITE,
                retry_after: retry_after(&response),
            }
            .into());
        }
        _ => {
            return Err(VideoError::Unreadable {
                why: format!("腾讯元宝返回 HTTP {status}"),
                answer: read_body(&mut response, cfg.limits.api_bytes).unwrap_or_default(),
            });
        }
    }
    let raw = read_body(&mut response, cfg.limits.api_bytes).map_err(|e| match e {
        FetchError::Network(_) => VideoError::Fatal(e),
        _ => VideoError::Missed("腾讯元宝的回答太大，它的接口可能改了".into()),
    })?;
    serde_json::from_slice(&raw).map_err(|_| VideoError::Unreadable {
        why: "腾讯元宝的回答解析不出，它的接口可能改了".into(),
        answer: raw.clone(),
    })
}

/// 元宝错误对象里的 `code`：参照源码按字符串比，数字和字符串都接。
fn error_code(error: &Value) -> Option<String> {
    match error.get("code")? {
        Value::String(s) => Some(plain_snippet(s)),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
