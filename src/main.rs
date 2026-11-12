//! 入口：子命令照协议 §3.2（`canonicalize`、`fetch`、`health`），按方法分派（§2.3.1），stdout 只写一个信封。

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use serde::Serialize;
use usuyo_fetch_wechat_channels::credentials::{self, YuanbaoCookies};
use usuyo_fetch_wechat_channels::{Config, FetchError, canonicalize, fetch, health};
use usuyo_plugin_kit::fetch::{Call, USAGE};
use usuyo_plugin_kit::protocol::Envelope;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::from(1)
        }
    }
}

/// 分派到清单里的方法：`feed-info` 不带账号取作者、描述、封面，`yuanbao` 用元宝的登录只取视频。
/// 浏览器脚本、手机、电脑客户端那几种还没做，宿主不会调到这里。
fn run() -> anyhow::Result<u8> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::production();
    let Some(call) = Call::parse(&args) else {
        return Ok(usage());
    };
    let method = call.method().map(str::to_owned);
    match (method.as_deref(), call) {
        (_, Call::Canonicalize { url }) => print(canonicalize(&cfg, &url)),
        (
            Some("feed-info"),
            Call::Fetch {
                input, out, want, ..
            },
        ) => print(fetch::feed_info(&cfg, &input, &out, want.as_deref())),
        (
            Some("yuanbao"),
            Call::Fetch {
                input, out, want, ..
            },
        ) => print(
            cookies().and_then(|c| fetch::yuanbao(&cfg, &input, &out, want.as_deref(), c.as_ref())),
        ),
        (_, Call::Health { probe }) => {
            // 不带 --probe 时宿主不传凭据，这里也不读
            let cookies = if probe { cookies() } else { Ok(None) };
            print(cookies.and_then(|c| health::health(&cfg, probe, c.as_ref())))
        }
        (method, _) => print::<()>(Err(FetchError::InvalidArgs(format!(
            "this call does not apply to method {method:?}"
        )))),
    }
}

/// 宿主传了凭据时（协议 §6）读出元宝的 cookie。
fn cookies() -> Result<Option<YuanbaoCookies>, FetchError> {
    match std::env::var_os("USUYO_CREDENTIALS") {
        Some(path) => credentials::load(&PathBuf::from(path)),
        None => Ok(None),
    }
}

fn usage() -> u8 {
    eprintln!("{USAGE}");
    2
}

/// 把结果写成 stdout 上的那一个信封，返回退出码。
fn print<T: Serialize>(result: Result<T, FetchError>) -> anyhow::Result<u8> {
    let envelope = match result {
        Ok(body) => Envelope::Ok(body),
        Err(e) => Envelope::Err(e.into_failure()),
    };
    let json = serde_json::to_string(&envelope).context("cannot write the envelope")?;
    println!("{json}");
    Ok(if envelope.exit_code() == 0 { 0 } else { 1 })
}
