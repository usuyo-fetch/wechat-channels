//! 清单经宿主的 `discover` 校验；可执行文件只跑不发请求的那几种调用：
//! 默认地址是真的腾讯主机，所以要发请求的调用都在 `fetch.rs` 里对着本机服务跑库函数。

use std::path::PathBuf;
use std::process::Command;

use usuyo_plugin::protocol::Canonicalized;
use usuyo_plugin::{CallLimits, CallOptions, HostConfig, PluginHost, discover};

const BIN: &str = env!("CARGO_BIN_EXE_usuyo-fetch-wechat-channels");

fn install() -> (tempfile::TempDir, PluginHost) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/usuyo-fetch/wechat-channels/0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml");
    std::fs::copy(manifest, dir.join("usuyo-plugin.yaml")).unwrap();
    std::fs::copy(BIN, dir.join("usuyo-fetch-wechat-channels")).unwrap();
    let found = discover(&tmp.path().join("plugins")).unwrap();
    assert!(found.rejected.is_empty(), "{:?}", found.rejected);
    assert_eq!(found.fetch.len(), 1);
    let host = PluginHost::new(
        HostConfig {
            work_dir: tmp.path().join("work"),
            cache_dir: tmp.path().join("cache"),
            locale: "zh-CN".into(),
            limits: CallLimits::default(),
        },
        found.fetch,
    );
    (tmp, host)
}

#[test]
fn manifest_is_accepted_and_matches_both_shapes() {
    let (_tmp, host) = install();
    for link in [
        "https://weixin.qq.com/sph/AbC123",
        "https://channels.weixin.qq.com/finder-preview/pages/sph?id=AbC123",
    ] {
        let url = url::Url::parse(link).unwrap();
        let found = host.candidates(&url);
        assert_eq!(found.len(), 1, "{link}");
        assert_eq!(found[0].id().as_str(), "usuyo-fetch/wechat-channels");
    }
    assert!(
        host.candidates(&url::Url::parse("https://www.bilibili.com/video/BV1").unwrap())
            .is_empty()
    );
}

#[tokio::test]
async fn host_canonicalize_of_a_non_matching_page_needs_no_network() {
    let (_tmp, host) = install();
    // match 的 glob 命中，但不是分享页：离线就能回 match: false
    let url = url::Url::parse("https://channels.weixin.qq.com/finder-preview/pages/feed?id=AbC123")
        .unwrap();
    let plugin = host.candidates(&url)[0];
    let got = host
        .canonicalize(plugin, &url, &CallOptions::default())
        .await
        .unwrap();
    assert_eq!(got, Canonicalized::NoMatch);
}

#[test]
fn health_without_probe_sends_nothing() {
    let out = Command::new(BIN)
        .arg("health")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["ready"], true);
    assert_eq!(json["protocol"], 1);
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["login"], serde_json::json!([]));
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["fetch", "https://weixin.qq.com/sph/A"][..],
        &["list", "x"],
        &["health", "--x"],
        &[],
    ] {
        let out = Command::new(BIN).args(args).env_clear().output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn binary_answers_foreign_links_offline() {
    let out = Command::new(BIN)
        .args(["canonicalize", "https://example.com/a"])
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        r#"{"ok":true,"match":false}"#
    );

    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .args(["fetch", "https://example.com/a", "--out"])
        .arg(dir.path())
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["error"]["code"], "unsupported-input");
}

#[test]
fn binary_reports_a_bad_credentials_file_without_its_content() {
    let dir = tempfile::tempdir().unwrap();
    let cred = dir.path().join("cred.json");
    std::fs::write(
        &cred,
        r#"{"storageState": {"cookies": [{"name": "a", "value": 98765}]}}"#,
    )
    .unwrap();
    // 用不匹配的链接：就算读凭据的顺序变了，也不会向真的主机发请求
    let out = Command::new(BIN)
        .args(["fetch", "https://example.com/a", "--out"])
        .arg(dir.path())
        .env_clear()
        .env("USUYO_CREDENTIALS", &cred)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("internal"), "{text}");
    assert!(!text.contains("98765"), "{text}");
}
