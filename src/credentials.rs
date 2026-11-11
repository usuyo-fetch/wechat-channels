//! 宿主传来的元宝登录（协议 §6）：`USUYO_CREDENTIALS` 指向的 `storageState` JSON。
//!
//! 只取能发给 `yuanbao.tencent.com` 的 cookie，拼成一个 Cookie 头。值不进日志、不进错误信息、
//! 不写盘；这里的类型都不派生 `Debug`。

use std::fmt;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;

use crate::{FetchError, YUANBAO_SITE};

/// 凭据文件最大多少：一个站点的 cookie 远小于这个数。
const MAX_FILE: u64 = 1 << 20;

/// 发给元宝的 Cookie 头的值。
pub struct YuanbaoCookies(String);

impl YuanbaoCookies {
    pub(crate) fn header(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for YuanbaoCookies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("YuanbaoCookies(<redacted>)")
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    sites: Vec<SiteRef>,
    #[serde(rename = "storageState")]
    storage_state: StorageState,
}

#[derive(Deserialize)]
struct SiteRef {
    site: String,
}

#[derive(Deserialize)]
struct StorageState {
    #[serde(default)]
    cookies: Vec<Cookie>,
}

#[derive(Deserialize)]
struct Cookie {
    name: String,
    value: String,
    domain: String,
    /// Unix 秒；`-1` 或没有是会话 cookie。
    #[serde(default)]
    expires: Option<f64>,
}

/// 读凭据文件。文件里没有元宝这个站点时是 `None`：这次调用不带元宝账号。
pub fn load(path: &Path) -> Result<Option<YuanbaoCookies>, FetchError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_FILE + 1).read_to_end(&mut bytes))
        .map_err(|e| FetchError::Internal(format!("cannot read the credentials file: {e}")))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(FetchError::Internal("credentials file is too large".into()));
    }
    // serde 的错误信息可能带出字段值，所以只报一句固定的话
    let file: File = serde_json::from_slice(&bytes).map_err(|_| {
        FetchError::Internal("credentials file is not storageState JSON (protocol §6)".into())
    })?;
    from_file(file, jiff::Timestamp::now().as_second())
}

fn from_file(file: File, now: i64) -> Result<Option<YuanbaoCookies>, FetchError> {
    if !file.sites.iter().any(|s| s.site == YUANBAO_SITE) {
        return Ok(None);
    }
    let mut pairs = Vec::new();
    for c in &file.storage_state.cookies {
        if !domain_matches(&c.domain) {
            continue;
        }
        if c.expires.is_some_and(|t| t > 0.0 && t < now as f64) {
            continue;
        }
        if !sendable(&c.name, &c.value) {
            eprintln!(
                "skipped cookie `{}`: its name or value cannot go in a Cookie header",
                c.name.escape_debug()
            );
            continue;
        }
        pairs.push(format!("{}={}", c.name, c.value));
    }
    if pairs.is_empty() {
        return Err(FetchError::LoginRequired(format!(
            "no unexpired cookies for {YUANBAO_SITE} in the credentials"
        )));
    }
    Ok(Some(YuanbaoCookies(pairs.join("; "))))
}

/// cookie 的 `domain` 能不能发给元宝：等于站点或是它的上级域名，不接受只有一段的公共后缀。
fn domain_matches(domain: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    d.contains('.') && (d == YUANBAO_SITE || YUANBAO_SITE.ends_with(&format!(".{d}")))
}

/// RFC 6265 的 cookie-name 和 cookie-value：不能有空白、控制字符、分号、逗号、引号、反斜杠。
fn sendable(name: &str, value: &str) -> bool {
    let token = |b: u8| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b);
    let octet = |b: u8| b.is_ascii_graphic() && !b"\",;\\".contains(&b);
    !name.is_empty() && name.bytes().all(token) && value.bytes().all(octet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(json: &str) -> File {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn keeps_only_cookies_for_the_site() {
        let f = file(
            r#"{"sites":[{"site":"yuanbao.tencent.com","source":"dedicated"}],
                "storageState":{"cookies":[
                  {"name":"a","value":"1","domain":".tencent.com","expires":-1},
                  {"name":"b","value":"2","domain":"yuanbao.tencent.com","expires":4000000000},
                  {"name":"old","value":"3","domain":"yuanbao.tencent.com","expires":1000},
                  {"name":"other","value":"4","domain":".qq.com"},
                  {"name":"suffix","value":"5","domain":".com"},
                  {"name":"bad","value":"x;y","domain":"yuanbao.tencent.com"}
                ],"origins":[]}}"#,
        );
        let got = from_file(f, 2_000_000_000).unwrap().unwrap();
        assert_eq!(got.header(), "a=1; b=2");
        assert_eq!(format!("{got:?}"), "YuanbaoCookies(<redacted>)");
    }

    #[test]
    fn other_sites_mean_no_login() {
        let f = file(r#"{"sites":[{"site":"www.douyin.com"}],"storageState":{"cookies":[]}}"#);
        assert!(from_file(f, 0).unwrap().is_none());
    }

    #[test]
    fn all_expired_is_login_required() {
        let f = file(
            r#"{"sites":[{"site":"yuanbao.tencent.com"}],
                "storageState":{"cookies":[{"name":"a","value":"1","domain":".tencent.com","expires":5}]}}"#,
        );
        assert!(matches!(
            from_file(f, 10),
            Err(FetchError::LoginRequired(_))
        ));
    }

    #[test]
    fn parse_errors_do_not_echo_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cred.json");
        std::fs::write(
            &path,
            r#"{"sites":[],"storageState":{"cookies":[{"name":"a","value":12345678}]}}"#,
        )
        .unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(!err.contains("12345678"), "{err}");
    }
}
