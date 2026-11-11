//! 入口：子命令照协议 §3.2（`canonicalize`、`fetch`、`health`），stdout 只写一个信封。

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use serde::Serialize;
use usuyo_fetch_wechat_channels::credentials::{self, YuanbaoCookies};
use usuyo_fetch_wechat_channels::{Config, FetchError, canonicalize, fetch, health};
use usuyo_plugin::protocol::{Envelope, OutputKind};

const USAGE: &str = "usage: usuyo-fetch-wechat-channels canonicalize <url> | fetch <url> --out <dir> [--want a,b] | health [--probe]";

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> anyhow::Result<u8> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = Config::production();
    match args.first().map(String::as_str) {
        Some("canonicalize") if args.len() == 2 => print(canonicalize(&cfg, &args[1])),
        Some("fetch") => match parse_fetch(&args[1..]) {
            Some((url, out, want)) => print(
                cookies().and_then(|c| fetch::fetch(&cfg, &url, &out, want.as_deref(), c.as_ref())),
            ),
            None => Ok(usage()),
        },
        Some("health") if args.len() <= 2 && args.get(1).is_none_or(|a| a == "--probe") => {
            let probe = args.len() == 2;
            // 不带 --probe 时宿主不传凭据，这里也不读
            let cookies = if probe { cookies() } else { Ok(None) };
            print(cookies.and_then(|c| health::health(&cfg, probe, c.as_ref())))
        }
        _ => Ok(usage()),
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

fn parse_fetch(args: &[String]) -> Option<(String, PathBuf, Option<Vec<OutputKind>>)> {
    let (url, rest) = args.split_first()?;
    let mut out = None;
    let mut want = None;
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(it.next()?)),
            "--want" => {
                want = Some(
                    it.next()?
                        .split(',')
                        .map(OutputKind::parse)
                        .collect::<Option<Vec<_>>>()?,
                );
            }
            _ => return None,
        }
    }
    Some((url.clone(), out?, want))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_args() {
        let args: Vec<String> = [
            "https://weixin.qq.com/sph/A1",
            "--out",
            "/tmp/o",
            "--want",
            "text,media:video",
        ]
        .map(String::from)
        .to_vec();
        let (_, out, want) = parse_fetch(&args).unwrap();
        assert_eq!(out, PathBuf::from("/tmp/o"));
        assert_eq!(want, Some(vec![OutputKind::Text, OutputKind::Video]));
        let bad: Vec<String> = ["https://weixin.qq.com/sph/A1", "--want", "video"]
            .map(String::from)
            .to_vec();
        assert!(parse_fetch(&bad).is_none());
    }
}
