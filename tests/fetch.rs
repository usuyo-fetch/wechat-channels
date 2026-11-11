//! canonicalize 和 fetch 对着本机的假服务跑：分享页接口和 CDN 一个服务，元宝一个服务。
//! 固定回答照 ParseHub `wxchannels.py` 的解析和设计 §3.2 的描述写；匿名那一种的形状是推断。

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{JPEG, Reply, Request, Server, config, fixture, mp4};
use usuyo_fetch_wechat_channels::credentials::{self, YuanbaoCookies};
use usuyo_fetch_wechat_channels::fetch::{NO_LOGIN_REASON, fetch};
use usuyo_fetch_wechat_channels::health::health;
use usuyo_fetch_wechat_channels::{Config, FetchError, canonicalize, identity};
use usuyo_plugin::protocol::{
    Access, Canonicalized, Completeness, Envelope, ErrorCode, FetchOutput, Identity, MediaKind,
    OutputKind,
};

const LINK: &str = "https://weixin.qq.com/sph/AbC123";
const FEED_ID: &str = "export/UzFfFixtureExportId0001";
const SENTINEL: &str = "SENTINEL-c00kie-v4lue";

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

/// 回答里的 `{media}` 换成这个服务自己的地址。
fn media_base(req: &Request) -> String {
    format!("http://{}", req.header("host").unwrap())
}

struct World {
    channels: Server,
    yuanbao: Server,
    cfg: Config,
    out: tempfile::TempDir,
}

struct Setup {
    anonymous: Handler,
    token: Handler,
    video: Handler,
    yuanbao: Handler,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            anonymous: Arc::new(|r| {
                Reply::json(200, fixture("feed-anonymous.json", &media_base(r)))
            }),
            token: Arc::new(|r| Reply::json(200, fixture("feed-token.json", &media_base(r)))),
            video: Arc::new(|_| Reply::bytes("video/mp4", mp4())),
            yuanbao: Arc::new(|_| Reply::json(200, fixture("yuanbao-ok.json", "http://unused"))),
        }
    }
}

impl Setup {
    fn start(self) -> World {
        let Setup {
            anonymous,
            token,
            video,
            yuanbao,
        } = self;
        let channels = Server::start(move |r| match (r.method.as_str(), r.path()) {
            ("POST", "/finder-preview/api/feed/get_feed_info")
                if r.body.contains("generalToken") =>
            {
                token(r)
            }
            ("POST", "/finder-preview/api/feed/get_feed_info") => anonymous(r),
            ("GET", "/cover.jpg") => Reply::bytes("image/jpeg", JPEG),
            ("GET", "/video.mp4") => video(r),
            _ => Reply::json(404, "{}"),
        });
        let yb = Server::start(move |r| match (r.method.as_str(), r.path()) {
            ("POST", "/api/weixin/get_parse_result") => yuanbao(r),
            _ => Reply::json(404, "{}"),
        });
        let cfg = config(&channels, &yb);
        World {
            channels,
            yuanbao: yb,
            cfg,
            out: tempfile::tempdir().unwrap(),
        }
    }
}

impl World {
    fn fetch(
        &self,
        want: Option<&[OutputKind]>,
        cookies: Option<&YuanbaoCookies>,
    ) -> Result<FetchOutput, FetchError> {
        fetch(&self.cfg, LINK, self.out.path(), want, cookies)
    }

    fn files(&self) -> Vec<String> {
        let mut files = Vec::new();
        walk(self.out.path(), self.out.path(), &mut files);
        files.sort();
        files
    }
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        assert!(!kind.is_symlink(), "{:?}", entry.path());
        if kind.is_dir() {
            walk(root, &entry.path(), files);
        } else {
            assert!(kind.is_file(), "{:?}", entry.path());
            let rel = entry.path().strip_prefix(root).unwrap().to_owned();
            files.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

/// 照宿主传凭据的样子写一个 storageState 文件再读回来（协议 §6）。
fn cookies() -> YuanbaoCookies {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    let json = format!(
        r#"{{"sites":[{{"site":"yuanbao.tencent.com","source":"dedicated"}}],
            "storageState":{{"cookies":[
              {{"name":"yb_session","value":"{SENTINEL}","domain":".yuanbao.tencent.com","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}}
            ],"origins":[]}}}}"#
    );
    std::fs::write(&path, json).unwrap();
    credentials::load(&path).unwrap().unwrap()
}

fn code(e: FetchError) -> ErrorCode {
    e.into_failure().code
}

#[test]
fn canonicalize_uses_the_feed_id() {
    let w = Setup::default().start();
    let got = canonicalize(&w.cfg, LINK).unwrap();
    assert_eq!(
        got,
        Canonicalized::Match {
            identity: identity("AbC123", Some(FEED_ID)),
            resolved_url: None,
        }
    );
    let Canonicalized::Match {
        identity:
            Identity::Platform {
                platform,
                content_id,
                url,
            },
        ..
    } = got
    else {
        panic!()
    };
    assert_eq!(platform.as_str(), "wechat-channels");
    assert_eq!(content_id, FEED_ID);
    assert_eq!(url.as_deref(), Some(LINK));

    // 匿名请求：带 shortUri，不带 cookie，只发给分享页的接口
    let reqs = w.channels.requests();
    assert_eq!(reqs.len(), 1);
    assert!(
        reqs[0].body.contains(r#""shortUri":"AbC123""#),
        "{}",
        reqs[0].body
    );
    assert!(reqs[0].header("cookie").is_none());
    assert!(reqs[0].header("origin").is_some());
    assert!(reqs[0].header("referer").is_some());
    assert!(w.yuanbao.requests().is_empty());
}

#[test]
fn canonicalize_falls_back_to_the_short_code() {
    let w = Setup {
        anonymous: Arc::new(|r| {
            Reply::json(200, fixture("feed-anonymous-thin.json", &media_base(r)))
        }),
        ..Setup::default()
    }
    .start();
    let pages = "https://channels.weixin.qq.com/finder-preview/pages/sph?id=AbC123&scene=1";
    let Canonicalized::Match { identity, .. } = canonicalize(&w.cfg, pages).unwrap() else {
        panic!()
    };
    assert_eq!(
        identity,
        usuyo_fetch_wechat_channels::identity("AbC123", None)
    );
    let Identity::Platform { content_id, .. } = identity else {
        panic!()
    };
    assert_eq!(content_id, "sph-AbC123");
}

#[test]
fn canonicalize_offline_is_a_network_error() {
    let w = Setup::default().start();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = w.cfg.clone();
    cfg.endpoints.channels =
        url::Url::parse(&format!("http://{}", closed.local_addr().unwrap())).unwrap();
    drop(closed);
    // 宿主照协议 §3.1 拿链接本身建 web: 身份；插件自己不报 web 身份
    assert_eq!(
        code(canonicalize(&cfg, LINK).unwrap_err()),
        ErrorCode::Network
    );
}

#[test]
fn canonicalize_table_from_testdata() {
    let w = Setup::default().start();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/canonicalize.json");
    let table: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    for case in table["cases"].as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let got = canonicalize(&w.cfg, input).unwrap();
        if case["match"].as_bool().unwrap() {
            let code = case["shortCode"].as_str().unwrap();
            assert_eq!(
                got,
                Canonicalized::Match {
                    identity: identity(code, Some(FEED_ID)),
                    resolved_url: None
                },
                "{input}"
            );
            let Canonicalized::Match {
                identity: Identity::Platform { url, .. },
                ..
            } = got
            else {
                panic!()
            };
            assert_eq!(url.as_deref(), case["url"].as_str(), "{input}");
        } else {
            assert_eq!(got, Canonicalized::NoMatch, "{input}");
        }
    }
}

#[test]
fn anonymous_fetch_is_partial() {
    let w = Setup::default().start();
    let out = w.fetch(None, None).unwrap();
    assert_eq!(out.identity, Some(identity("AbC123", Some(FEED_ID))));
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    assert_eq!(out.missing_reason.as_deref(), Some(NO_LOGIN_REASON));
    assert!(
        out.text
            .as_deref()
            .unwrap()
            .starts_with("手冲咖啡：三段式注水")
    );
    assert_eq!(out.author.as_ref().unwrap().name, "咖啡日常");
    assert_eq!(out.images.len(), 1);
    assert_eq!(
        std::fs::read(w.out.path().join(out.images[0].as_str())).unwrap(),
        JPEG
    );
    assert!(out.media.is_empty());
    assert_eq!(out.evidence.access, Access::PublicWeb);
    assert_eq!(out.evidence.site, None);
    assert_eq!(out.evidence.status, Some(200));
    assert_eq!(w.files(), ["feed.json", "images/cover.jpg", "meta.json"]);
    assert!(w.yuanbao.requests().is_empty());
}

#[test]
fn thin_anonymous_answer_still_stores_author_and_cover() {
    let w = Setup {
        anonymous: Arc::new(|r| {
            Reply::json(200, fixture("feed-anonymous-thin.json", &media_base(r)))
        }),
        ..Setup::default()
    }
    .start();
    let out = w.fetch(None, None).unwrap();
    assert_eq!(out.missing, [OutputKind::Text, OutputKind::Video]);
    assert!(
        out.missing_reason
            .as_deref()
            .unwrap()
            .contains("分享页没有给出描述")
    );
    assert_eq!(out.images.len(), 1);
    assert!(out.author.is_some());
}

#[test]
fn with_yuanbao_login_saves_the_video() {
    let w = Setup::default().start();
    let cookies = cookies();
    let out = w.fetch(None, Some(&cookies)).unwrap();
    assert_eq!(
        out.completeness,
        Completeness::Full,
        "{:?}",
        out.missing_reason
    );
    assert!(out.missing.is_empty());
    assert_eq!(out.media.len(), 1);
    assert_eq!(out.media[0].kind, MediaKind::Video);
    assert_eq!(out.media[0].mime.as_deref(), Some("video/mp4"));
    assert_eq!(
        std::fs::read(w.out.path().join(out.media[0].path.as_str())).unwrap(),
        mp4()
    );
    assert_eq!(out.evidence.access, Access::SessionApi);
    assert_eq!(out.evidence.site.as_deref(), Some("yuanbao.tencent.com"));
    // 身份只看匿名回答，登没登录都一样
    assert_eq!(out.identity, Some(identity("AbC123", Some(FEED_ID))));

    // 元宝的请求：带 cookie，body 是重建的分享链接
    let yb = w.yuanbao.requests();
    assert_eq!(yb.len(), 1);
    assert!(yb[0].header("cookie").unwrap().contains(SENTINEL));
    let body: serde_json::Value = serde_json::from_str(&yb[0].body).unwrap();
    assert_eq!(body["type"], "video_channel_url");
    assert_eq!(body["url"], LINK);
    assert_eq!(body["scene"], 1);

    // 带 token 的那次：body 照 ParseHub，Referer 是元宝给的 playable_url
    let ch = w.channels.requests();
    let token_req = ch.iter().find(|r| r.body.contains("generalToken")).unwrap();
    let body: serde_json::Value = serde_json::from_str(&token_req.body).unwrap();
    assert_eq!(body["baseReq"]["generalToken"], "FIXTURE-TEMP-TOKEN");
    assert_eq!(body["exportId"], FEED_ID);
    assert!(
        token_req
            .header("referer")
            .unwrap()
            .contains("token=FIXTURE-TEMP-TOKEN")
    );
    assert!(token_req.target.contains("_rid="));

    // cookie 值只到元宝：分享页和 CDN 的请求里、输出里、文件里都没有
    for r in &ch {
        assert!(r.header("cookie").is_none(), "{}", r.dump());
        assert!(!r.dump().contains(SENTINEL), "{}", r.dump());
    }
    let envelope = serde_json::to_string(&Envelope::Ok(&out)).unwrap();
    assert!(!envelope.contains(SENTINEL));
    for f in w.files() {
        let bytes = std::fs::read(w.out.path().join(&f)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains(SENTINEL), "{f}");
        assert!(!text.contains("FIXTURE-TEMP-TOKEN"), "{f}");
    }
}

#[test]
fn output_dir_contents() {
    let w = Setup::default().start();
    let out = w.fetch(None, Some(&cookies())).unwrap();
    assert_eq!(
        w.files(),
        [
            "feed.json",
            "images/cover.jpg",
            "media/video.mp4",
            "meta.json"
        ]
    );
    for p in out.images.iter().chain(out.media.iter().map(|m| &m.path)) {
        assert!(w.out.path().join(p.as_str()).is_file(), "{p}");
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(w.out.path().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["shortCode"], "AbC123");
    assert_eq!(meta["feedId"], FEED_ID);
    assert_eq!(meta["exportId"], FEED_ID);
    assert_eq!(meta["shareUrl"], LINK);
    // feed.json 是匿名回答的原样
    let raw = std::fs::read_to_string(w.out.path().join("feed.json")).unwrap();
    assert!(raw.contains("likeCount"));

    // 信封照协议的类型能读回来
    let json = serde_json::to_value(Envelope::Ok(&out)).unwrap();
    assert_eq!(json["ok"], true);
    let back: FetchOutput = serde_json::from_value(json).unwrap();
    assert_eq!(back, out);
}

#[test]
fn want_limits_the_work() {
    let w = Setup::default().start();
    let out = w
        .fetch(Some(&[OutputKind::Text]), Some(&cookies()))
        .unwrap();
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.images.is_empty());
    assert!(w.files().is_empty(), "{:?}", w.files());
    // 不要视频就不碰元宝账号
    assert!(w.yuanbao.requests().is_empty());
    assert_eq!(out.evidence.access, Access::PublicWeb);
}

#[test]
fn only_video_without_login_is_login_required() {
    let w = Setup::default().start();
    let failure = w
        .fetch(Some(&[OutputKind::Video]), None)
        .unwrap_err()
        .into_failure();
    assert_eq!(failure.code, ErrorCode::LoginRequired);
    assert_eq!(failure.site.as_deref(), Some("yuanbao.tencent.com"));
}

#[test]
fn expired_login() {
    let cases: [Handler; 3] = [
        Arc::new(|_| Reply::json(401, "{}")),
        Arc::new(|_| Reply::json(200, fixture("yuanbao-expired.json", ""))),
        Arc::new(|_| Reply::json(200, fixture("yuanbao-expired-string.json", ""))),
    ];
    for yuanbao in cases {
        let w = Setup {
            yuanbao,
            ..Setup::default()
        }
        .start();
        let failure = w.fetch(None, Some(&cookies())).unwrap_err().into_failure();
        assert_eq!(failure.code, ErrorCode::LoginRequired);
        assert_eq!(failure.site.as_deref(), Some("yuanbao.tencent.com"));
        assert!(failure.hint.as_deref().unwrap().contains("腾讯元宝"));
        assert!(!serde_json::to_string(&failure).unwrap().contains(SENTINEL));
    }
}

#[test]
fn yuanbao_rate_limit_pauses_the_plugin() {
    let w = Setup {
        yuanbao: Arc::new(|_| Reply::json(429, "{}")),
        ..Setup::default()
    }
    .start();
    assert_eq!(
        code(w.fetch(None, Some(&cookies())).unwrap_err()),
        ErrorCode::RateLimited
    );
}

#[test]
fn other_video_failures_keep_the_metadata() {
    let cases: [(&str, Setup); 5] = [
        (
            "腾讯元宝没有给出这条视频的地址",
            Setup {
                yuanbao: Arc::new(|_| Reply::json(200, r#"{"code":0,"data":{}}"#)),
                ..Setup::default()
            },
        ),
        (
            "腾讯元宝的回答解析不出",
            Setup {
                yuanbao: Arc::new(|_| Reply::json(200, "<html>")),
                ..Setup::default()
            },
        ),
        (
            "不是能直接播放的 MP4",
            Setup {
                video: Arc::new(|_| {
                    Reply::bytes("video/mp4", b"\x00\x01encrypted-or-html".repeat(10))
                }),
                ..Setup::default()
            },
        ),
        (
            "视频超过 1 MiB",
            Setup {
                video: Arc::new(|_| Reply::bytes("video/mp4", vec![0u8; (1 << 20) + 1])),
                ..Setup::default()
            },
        ),
        (
            "不在允许的主机里",
            Setup {
                token: Arc::new(|r| {
                    let body = fixture("feed-token.json", &media_base(r)).replace(
                        &format!("{}/video.mp4", media_base(r)),
                        "http://example.invalid/video.mp4",
                    );
                    Reply::json(200, body)
                }),
                ..Setup::default()
            },
        ),
    ];
    for (reason, setup) in cases {
        let w = setup.start();
        let out = w.fetch(None, Some(&cookies())).unwrap();
        assert_eq!(out.completeness, Completeness::Partial, "{reason}");
        assert_eq!(out.missing, [OutputKind::Video], "{reason}");
        let got = out.missing_reason.unwrap();
        assert!(got.contains(reason), "{reason}: {got}");
        assert!(out.text.is_some() && !out.images.is_empty());
        assert!(
            !w.files().iter().any(|f| f.starts_with("media/")),
            "{reason}: {:?}",
            w.files()
        );
    }
}

#[test]
fn stalled_video_gives_up_and_keeps_the_rest() {
    let w = Setup {
        video: Arc::new(|_| Reply {
            stall_after: Some(64),
            ..Reply::bytes("video/mp4", mp4())
        }),
        ..Setup::default()
    }
    .start();
    let started = std::time::Instant::now();
    let out = w.fetch(None, Some(&cookies())).unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert_eq!(out.missing, [OutputKind::Video]);
    assert!(out.missing_reason.unwrap().contains("没有新数据"));
    assert!(!w.files().iter().any(|f| f.starts_with("media/")));
}

#[test]
fn malformed_answers_are_upstream_changed() {
    let bodies = [
        "not json".to_owned(),
        r#"{"data":{"feedInfo":{"description":"x"}}}"#.to_owned(),
        r#"{"errCode":0,"data":{}}"#.to_owned(),
        r#"{"errCode":0,"data":{"feedInfo":{}}}"#.to_owned(),
        r#"{"errCode":0,"data":{"feedInfo":{"description":["x"]}}}"#.to_owned(),
        r#"{"errCode":0,"data":{"feedInfo":{"description":"x","exportId":42}}}"#.to_owned(),
        r#"{"errCode":0,"data":{"feedInfo":{"description":"x","exportId":"has space"}}}"#
            .to_owned(),
        r#"{"errCode":-1,"errMsg":"unknown","data":{}}"#.to_owned(),
    ];
    for body in bodies {
        let b = body.clone();
        let w = Setup {
            anonymous: Arc::new(move |_| Reply::json(200, b.clone())),
            ..Setup::default()
        }
        .start();
        assert_eq!(
            code(w.fetch(None, None).unwrap_err()),
            ErrorCode::UpstreamChanged,
            "{body}"
        );
        assert_eq!(
            code(canonicalize(&w.cfg, LINK).unwrap_err()),
            ErrorCode::UpstreamChanged,
            "{body}"
        );
        assert!(w.files().is_empty(), "{body}");
    }
}

#[test]
fn feed_level_failures() {
    let unavailable = Setup {
        anonymous: Arc::new(|_| {
            Reply::json(200, r#"{"errCode":0,"data":{"errMsg":{"title":"<p>该视频已被删除</p>"},"feedInfo":{}}}"#)
        }),
        ..Setup::default()
    }
    .start();
    assert_eq!(
        code(unavailable.fetch(None, None).unwrap_err()),
        ErrorCode::NotFound
    );

    let limited = Setup {
        anonymous: Arc::new(|_| Reply::json(429, "{}")),
        ..Setup::default()
    }
    .start();
    assert_eq!(
        code(limited.fetch(None, None).unwrap_err()),
        ErrorCode::RateLimited
    );

    let down = Setup {
        anonymous: Arc::new(|_| Reply::json(502, "")),
        ..Setup::default()
    }
    .start();
    assert_eq!(
        code(down.fetch(None, None).unwrap_err()),
        ErrorCode::Network
    );
}

#[test]
fn foreign_links_are_unsupported() {
    let w = Setup::default().start();
    let err = fetch(
        &w.cfg,
        "https://example.com/sph/AbC123",
        w.out.path(),
        None,
        None,
    )
    .unwrap_err();
    assert_eq!(code(err), ErrorCode::UnsupportedInput);
    assert_eq!(
        canonicalize(&w.cfg, "https://weixin.qq.com/s/AbC123").unwrap(),
        Canonicalized::NoMatch
    );
    assert!(w.channels.requests().is_empty());
}

#[test]
fn probe_reports_the_yuanbao_login() {
    let channels = Server::start(|_| Reply::json(404, "{}"));
    // health --probe 只发给元宝，分享页的接口一次也不碰
    for (reply, state) in [
        (r#"{"error":{"code":20000}}"#, "rejected"),
        (
            r#"{"error":{"code":"40001","message":"链接无效"}}"#,
            "verified",
        ),
        (r#"{"code":0,"data":{}}"#, "verified"),
    ] {
        let yb = Server::start(move |_| Reply::json(200, reply));
        let cfg = config(&channels, &yb);
        let got = health(&cfg, true, Some(&cookies())).unwrap();
        assert_eq!(got["login"][0]["site"], "yuanbao.tencent.com");
        assert_eq!(got["login"][0]["state"], state, "{reply}");
        assert_eq!(yb.requests().len(), 1);
    }
    let yb = Server::start(|_| Reply::json(401, "{}"));
    let got = health(&config(&channels, &yb), true, Some(&cookies())).unwrap();
    assert_eq!(got["login"][0]["state"], "rejected");

    // 没有登录时 --probe 也不发请求
    let idle = Server::start(|_| Reply::json(500, "{}"));
    let got = health(&config(&channels, &idle), true, None).unwrap();
    assert_eq!(got["login"], serde_json::json!([]));
    assert!(idle.requests().is_empty());
    assert!(channels.requests().is_empty());
}

#[test]
fn yuanbao_redirect_is_not_followed() {
    // 元宝回 302 指向分享页接口那台：cookie 不能跟过去
    let target = Server::start(|_| Reply::json(200, "{}"));
    let location = format!("{}/login", target.base);
    let w = Setup {
        yuanbao: Arc::new(move |_| Reply {
            status: 302,
            location: Some(location.clone()),
            ..Reply::json(302, "")
        }),
        ..Setup::default()
    }
    .start();
    let out = w.fetch(None, Some(&cookies())).unwrap();
    assert_eq!(out.missing, [OutputKind::Video]);
    assert!(out.missing_reason.unwrap().contains("HTTP 302"));
    assert!(target.requests().is_empty(), "{:?}", target.requests());
    for r in w.channels.requests() {
        assert!(!r.dump().contains(SENTINEL));
    }
}

#[test]
fn unreachable_yuanbao_keeps_the_metadata() {
    let w = Setup::default().start();
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = w.cfg.clone();
    cfg.endpoints.yuanbao =
        url::Url::parse(&format!("http://{}", closed.local_addr().unwrap())).unwrap();
    drop(closed);
    let out = fetch(&cfg, LINK, w.out.path(), None, Some(&cookies())).unwrap();
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    assert!(out.missing_reason.unwrap().contains("连不上腾讯元宝"));
    assert!(out.text.is_some() && !out.images.is_empty());

    // health --probe 连不上时照实报 network，不说登录有效
    let err = health(&cfg, true, Some(&cookies())).unwrap_err();
    assert_eq!(code(err), ErrorCode::Network);
}

#[test]
fn video_and_cover_failures_without_login_stay_partial() {
    // 封面下载失败、又没登录：仍有描述和作者，回 partial，不回 login-required
    let w = Setup {
        anonymous: Arc::new(|r| {
            Reply::json(
                200,
                fixture("feed-anonymous.json", &media_base(r))
                    .replace("/cover.jpg", "/missing.jpg"),
            )
        }),
        ..Setup::default()
    }
    .start();
    let out = w.fetch(None, None).unwrap();
    assert_eq!(out.missing, [OutputKind::Images, OutputKind::Video]);
}

#[test]
fn media_redirects_cannot_leave_the_host_list() {
    // 视频地址在名单里，CDN 跳去名单外的主机、跳去名单里但解析成回环的主机：都不连
    let other = Server::start(|_| Reply::bytes("video/mp4", mp4()));
    let port = other.base.rsplit(':').next().unwrap().to_owned();
    let cases = [
        (
            format!("http://localhost:{port}/video.mp4"),
            "not on the plugin's host list",
        ),
        (
            format!("http://loop.test:{port}/video.mp4"),
            "not a public address",
        ),
    ];
    for (location, why) in cases {
        let w = Setup {
            video: Arc::new(move |_| Reply {
                status: 302,
                location: Some(location.clone()),
                ..Reply::json(302, "")
            }),
            ..Setup::default()
        }
        .start();
        let out = w.fetch(None, Some(&cookies())).unwrap();
        assert_eq!(out.missing, [OutputKind::Video], "{why}");
        let reason = out.missing_reason.unwrap();
        assert!(
            reason.contains("视频下载失败") && reason.contains(why),
            "{reason}"
        );
        assert!(!w.files().iter().any(|f| f.starts_with("media/")));
    }
    assert!(other.requests().is_empty(), "{:?}", other.requests());
}
