//! `health`（协议 §3.10）。不带 `--probe` 时不发任何请求；元宝的登录状态由宿主按 cookie 自己算。

use usuyo_plugin_kit::protocol::{FetchHealth, LoginReport, LoginReportState};

use crate::credentials::YuanbaoCookies;
use crate::{Config, FetchError, YUANBAO_SITE, yuanbao};

pub fn health(
    cfg: &Config,
    probe: bool,
    cookies: Option<&YuanbaoCookies>,
) -> Result<FetchHealth, FetchError> {
    let mut h = FetchHealth::new(env!("CARGO_PKG_VERSION"));
    match (probe, cookies) {
        (false, _) => {}
        (true, None) => {
            h = h.check(
                "yuanbao-login",
                true,
                Some("没有腾讯元宝的登录，没有发请求；不登录也能取作者、描述和封面".into()),
                &["yuanbao"],
            );
        }
        (true, Some(cookies)) => {
            let accepted = yuanbao::probe(cfg, cookies)?;
            h.login.push(LoginReport {
                site: YUANBAO_SITE.into(),
                state: if accepted {
                    LoginReportState::Verified
                } else {
                    LoginReportState::Rejected
                },
            });
        }
    }
    Ok(h)
}
