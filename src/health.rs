//! `health`（协议 §3.10）。不带 `--probe` 时不发任何请求；元宝的登录状态由宿主按 cookie 自己算。

use serde_json::{Value, json};

use crate::credentials::YuanbaoCookies;
use crate::{Config, FetchError, YUANBAO_SITE, yuanbao};

pub fn health(
    cfg: &Config,
    probe: bool,
    cookies: Option<&YuanbaoCookies>,
) -> Result<Value, FetchError> {
    let mut login = Vec::new();
    let mut checks = Vec::new();
    match (probe, cookies) {
        (false, _) => {}
        (true, None) => checks.push(json!({
            "name": "yuanbao-login",
            "ok": true,
            "message": "没有腾讯元宝的登录，没有发请求；不登录也能取作者、描述和封面"
        })),
        (true, Some(cookies)) => {
            let accepted = yuanbao::probe(cfg, cookies)?;
            login.push(json!({
                "site": YUANBAO_SITE,
                "state": if accepted { "verified" } else { "rejected" }
            }));
        }
    }
    Ok(json!({
        "ready": true,
        "protocol": 1,
        "version": env!("CARGO_PKG_VERSION"),
        "upstream": [],
        "login": login,
        "checks": checks,
    }))
}
