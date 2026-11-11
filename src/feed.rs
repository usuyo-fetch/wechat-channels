//! 分享页的接口 `finder-preview/api/feed/get_feed_info`。
//!
//! 两种调法共用一个回答形状（照 ParseHub `wxchannels.py` 的解析，MIT，见 `yuanbao.rs` 顶部）：
//! `{errCode, errMsg, data: {errMsg: {title, content}, feedInfo: {...}, authorInfo: {nickname}}}`。
//!
//! 匿名那一种（带 `shortUri`）的请求体、回答里有没有 feed id、id 叫什么，参照源码里都没有，
//! 是按设计 §3.2 的描述写的推断；都收在 [`anonymous_request`] 和 [`feed_id`] 两处，实测后只改这里。

use std::hash::BuildHasher;

use serde_json::{Value, json};

use usuyo_plugin_kit::http::{Client, Request};

use crate::{Config, FetchError, api_client, plain_snippet, read_body, retry_after};

const FEED_API: &str = "/finder-preview/api/feed/get_feed_info";
/// feed id 最长多少：比实际的长得多，只挡住拼进来的长串。
const MAX_ID: usize = 512;

/// 匿名回答里取到的东西。
#[derive(Debug, Clone)]
pub struct AnonymousFeed {
    pub status: u16,
    /// 原样的回答，存成 `feed.json`，字段改了以后还能重新解析。
    pub raw: Vec<u8>,
    pub id: Option<String>,
    pub fields: FeedFields,
}

/// 两种调法都有的字段。
#[derive(Debug, Clone, Default)]
pub struct FeedFields {
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub author: Option<String>,
}

/// 解析失败的两种：分享页说这一条看不到，或回答的形状不对。
#[derive(Debug)]
pub enum Rejected {
    Unavailable(String),
    Malformed(String),
}

/// 一个解析过的回答。
pub struct Parsed {
    pub feed_info: serde_json::Map<String, Value>,
    pub fields: FeedFields,
}

/// 匿名调一次：不带任何 cookie，只带 Origin、Referer。
pub fn anonymous(cfg: &Config, code: &str) -> Result<AnonymousFeed, FetchError> {
    let client = api_client(cfg, &cfg.endpoints.channels);
    let mut response = anonymous_request(cfg, &client, code)
        .send()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = response.status();
    match status {
        200..=299 => {}
        429 => {
            return Err(FetchError::RateLimited {
                host: "channels.weixin.qq.com",
                retry_after: retry_after(&response),
            });
        }
        500..=599 => return Err(FetchError::Network(format!("feed API HTTP {status}"))),
        _ => {
            return Err(FetchError::UpstreamChanged(format!(
                "feed API HTTP {status}"
            )));
        }
    }
    let raw = read_body(&mut response, cfg.limits.api_bytes)?;
    let parsed = parse(&raw).map_err(|r| match r {
        Rejected::Unavailable(text) => FetchError::NotFound(text),
        Rejected::Malformed(why) => FetchError::UpstreamChanged(why),
    })?;
    let FeedFields {
        description,
        cover_url,
        author,
    } = &parsed.fields;
    if description.is_none() && cover_url.is_none() && author.is_none() {
        return Err(FetchError::UpstreamChanged(
            "feed answer has no description, cover or author".into(),
        ));
    }
    let id = feed_id(&parsed).map_err(FetchError::UpstreamChanged)?;
    Ok(AnonymousFeed {
        status,
        raw,
        id,
        fields: parsed.fields,
    })
}

/// 匿名请求（推断，见模块说明）：body 只有 `shortUri`；Referer 是浏览器打开分享链接后落到的页面。
fn anonymous_request<'c>(cfg: &Config, client: &'c Client, code: &str) -> Request<'c> {
    let base = &cfg.endpoints.channels;
    let page = page_url(cfg, "/finder-preview/pages/sph");
    let referer = format!("{page}?id={code}");
    let mut url = api_url(cfg);
    url.query_pairs_mut()
        .append_pair("_rid", &rid())
        .append_pair("_pageUrl", &page);
    client
        .post(&url, json!({"shortUri": code}).to_string())
        .header("Content-Type", "application/json")
        .header("Origin", &origin(base))
        .header("Referer", &referer)
        .timeout(cfg.limits.api_timeout)
}

/// 回答里的 feed id（推断）：先找 `feedInfo.exportId`，再找 `feedInfo.objectId`。
fn feed_id(parsed: &Parsed) -> Result<Option<String>, String> {
    for key in ["exportId", "objectId"] {
        if let Some(id) = opt_str(&parsed.feed_info, key)? {
            let ok = id.len() <= MAX_ID && id.bytes().all(|b| b.is_ascii_graphic());
            if !ok {
                return Err(format!("feedInfo.{key} is not a plain ASCII id"));
            }
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// 两种调法共用的解析。
pub fn parse(raw: &[u8]) -> Result<Parsed, Rejected> {
    let malformed = |why: &str| Rejected::Malformed(why.to_owned());
    let root: Value =
        serde_json::from_slice(raw).map_err(|_| malformed("feed answer is not JSON"))?;
    let err_code = root
        .get("errCode")
        .and_then(Value::as_i64)
        .ok_or_else(|| malformed("feed answer has no integer errCode"))?;
    if err_code != 0 {
        // 参照源码只把它当失败，没有列出各个码的意思，这里不猜
        let msg = root
            .get("errMsg")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return Err(Rejected::Malformed(format!(
            "feed errCode {err_code}: {}",
            plain_snippet(msg)
        )));
    }
    let data = root
        .get("data")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("feed answer has no data object"))?;
    if let Some(err) = data.get("errMsg").and_then(Value::as_object) {
        let text: Vec<String> = ["title", "content"]
            .iter()
            .filter_map(|k| err.get(*k).and_then(Value::as_str))
            .map(plain_snippet)
            .filter(|t| !t.is_empty())
            .collect();
        if !text.is_empty() {
            return Err(Rejected::Unavailable(text.join(" ")));
        }
    }
    let feed_info = data
        .get("feedInfo")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("feed answer has no data.feedInfo object"))?
        .clone();
    let author_info = match data.get("authorInfo") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(malformed("data.authorInfo is not an object")),
    };
    let fields = FeedFields {
        description: opt_str(&feed_info, "description").map_err(Rejected::Malformed)?,
        cover_url: opt_str(&feed_info, "coverUrl").map_err(Rejected::Malformed)?,
        author: opt_str(&author_info, "nickname").map_err(Rejected::Malformed)?,
    };
    Ok(Parsed { feed_info, fields })
}

/// 可选的字符串字段：没有、`null`、空串都算没有；别的类型算接口改了。
pub fn opt_str(map: &serde_json::Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim().to_owned()).filter(|s| !s.is_empty())),
        Some(_) => Err(format!("`{key}` is not a string")),
    }
}

pub fn api_url(cfg: &Config) -> url::Url {
    let mut url = cfg.endpoints.channels.clone();
    url.set_path(FEED_API);
    url
}

pub fn page_url(cfg: &Config, path: &str) -> String {
    let mut url = cfg.endpoints.channels.clone();
    url.set_path(path);
    url.into()
}

pub fn origin(base: &url::Url) -> String {
    base.origin().ascii_serialization()
}

/// 网页自己带的请求 id：十六进制秒数加 8 位随机十六进制（照 ParseHub）。
pub fn rid() -> String {
    let now = jiff::Timestamp::now().as_second();
    let random = std::collections::hash_map::RandomState::new().hash_one(now) as u32;
    format!("{now:x}-{random:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shapes() {
        let cases: [(&str, bool); 7] = [
            ("not json", false),
            (r#"{"data":{}}"#, false),
            (r#"{"errCode":"0","data":{}}"#, false),
            (r#"{"errCode":0}"#, false),
            (r#"{"errCode":0,"data":{"feedInfo":[]}}"#, false),
            (
                r#"{"errCode":0,"data":{"feedInfo":{"description":7}}}"#,
                false,
            ),
            (
                r#"{"errCode":0,"data":{"errMsg":{"title":"<b>内容已删除</b>"},"feedInfo":{}}}"#,
                true,
            ),
        ];
        for (raw, unavailable) in cases {
            match parse(raw.as_bytes()) {
                Err(Rejected::Unavailable(t)) if unavailable => assert_eq!(t, "内容已删除"),
                Err(Rejected::Malformed(_)) if !unavailable => {}
                other => panic!("{raw}: {:?}", other.map(|p| p.fields)),
            }
        }
    }

    #[test]
    fn rid_shape() {
        let rid = rid();
        let (secs, random) = rid.split_once('-').unwrap();
        assert!(u64::from_str_radix(secs, 16).is_ok());
        assert_eq!(random.len(), 8);
    }
}
