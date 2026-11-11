//! 取一条（协议 §3.6、§3.7）：匿名的元数据和封面，有元宝登录时再加视频。

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde_json::json;
use usuyo_plugin_kit::protocol::{
    Access, Author, Completeness, Evidence, FetchOutput, ItemKind, Media, MediaKind, OutputKind,
    RelPath, Rfc3339, TextFormat,
};

use crate::credentials::YuanbaoCookies;
use crate::feed::FeedFields;
use crate::yuanbao::{self, VideoError};
use crate::{Config, FetchError, YUANBAO_SITE, download, feed, identity, link};

/// 本插件能给的输出，和清单里的 `outputs` 一致。
pub const OUTPUTS: [OutputKind; 4] = [
    OutputKind::Text,
    OutputKind::Images,
    OutputKind::Video,
    OutputKind::Meta,
];

const VIDEO_FILE: &str = "media/video.mp4";

/// 没登录元宝时的 `missingReason`。
pub const NO_LOGIN_REASON: &str =
    "分享页没有给出视频地址；在设置里登录腾讯元宝后，可以用元宝账号取到视频";

/// `cookies` 是宿主传来的元宝登录；没有就只取匿名那部分。
pub fn fetch(
    cfg: &Config,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
    cookies: Option<&YuanbaoCookies>,
) -> Result<FetchOutput, FetchError> {
    let code = link::short_code(input)
        .ok_or_else(|| FetchError::UnsupportedInput(input.chars().take(200).collect()))?;
    let wanted: BTreeSet<OutputKind> =
        want.map_or_else(|| OUTPUTS.into(), |w| w.iter().copied().collect());
    if let Some(other) = wanted.iter().find(|w| !OUTPUTS.contains(w)) {
        return Err(FetchError::InvalidArgs(format!("cannot give `{other}`")));
    }

    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    let anon = feed::anonymous(cfg, &code)?;
    let mut reasons: Vec<String> = Vec::new();

    // 元宝那一段：只在要视频、而且宿主传了元宝登录时走
    let mut video: Option<(yuanbao::Exchange, yuanbao::VideoFeed)> = None;
    if wanted.contains(&OutputKind::Video) {
        match cookies {
            None => reasons.push(NO_LOGIN_REASON.into()),
            Some(cookies) => {
                match yuanbao::exchange(cfg, &code, cookies)
                    .and_then(|ex| yuanbao::video_feed(cfg, &ex).map(|v| (ex, v)))
                {
                    Ok(got) => video = Some(got),
                    Err(VideoError::Fatal(e)) => return Err(e),
                    Err(VideoError::Missed(why)) => {
                        eprintln!("video step skipped: {why}");
                        reasons.push(why);
                    }
                }
            }
        }
    }

    // 匿名回答缺的字段，用带 token 那次的补上
    let token_fields = video.as_ref().map(|(_, v)| &v.fields);
    let pick = |f: fn(&FeedFields) -> &Option<String>| {
        f(&anon.fields)
            .clone()
            .or_else(|| token_fields.and_then(|t| f(t).clone()))
    };
    let description = pick(|f| &f.description);
    let cover_url = pick(|f| &f.cover_url);
    let author = pick(|f| &f.author);

    let mut output = FetchOutput {
        identity: Some(identity(&code, anon.id.as_deref())),
        kind: ItemKind::Video,
        title: None,
        author: None,
        published_at: None,
        text: None,
        text_format: None,
        html: None,
        images: Vec::new(),
        media: Vec::new(),
        platform_tags: Vec::new(),
        comments: None,
        completeness: Completeness::Full,
        missing: Vec::new(),
        missing_reason: None,
        evidence: Evidence {
            fetched_at,
            access: if cookies.is_some() && wanted.contains(&OutputKind::Video) {
                Access::SessionApi
            } else {
                Access::PublicWeb
            },
            site: (cookies.is_some() && wanted.contains(&OutputKind::Video))
                .then(|| YUANBAO_SITE.into()),
            upstream: None,
            status: Some(anon.status),
        },
        credentials_update: None,
    };

    if wanted.contains(&OutputKind::Text) {
        output.text = description.clone();
        output.text_format = output.text.as_ref().map(|_| TextFormat::Plain);
        if output.text.is_none() {
            reasons.push("分享页没有给出描述".into());
        }
    }
    if wanted.contains(&OutputKind::Meta) {
        output.author = author.clone().map(|name| Author {
            name,
            platform_id: None,
            url: None,
        });
    }
    if wanted.contains(&OutputKind::Images) {
        match &cover_url {
            None => reasons.push("分享页没有给出封面".into()),
            Some(url) => match download::cover(cfg, url) {
                Ok((bytes, ext)) => {
                    let name = format!("images/cover.{ext}");
                    fs::create_dir_all(out.join("images"))
                        .map_err(|e| FetchError::Internal(format!("create images/: {e}")))?;
                    fs::write(out.join(&name), bytes)
                        .map_err(|e| FetchError::Internal(format!("write {name}: {e}")))?;
                    output.images.push(rel(&name)?);
                }
                Err(why) => reasons.push(why),
            },
        }
    }
    if let Some((_, v)) = &video {
        match download::video(cfg, &v.video_url, &out.join(VIDEO_FILE)) {
            Ok(_) => output.media.push(Media {
                path: rel(VIDEO_FILE)?,
                kind: MediaKind::Video,
                mime: Some("video/mp4".into()),
                lang: None,
                duration_sec: None,
                group: None,
            }),
            Err(why) => {
                eprintln!("video download skipped: {why}");
                reasons.push(why);
            }
        }
    }
    if wanted.contains(&OutputKind::Meta) {
        // 协议的输出没有短码、描述、封面地址这几栏，写进 meta.json，随快照留在 raw/。
        // 不写元宝的回答和临时 token。
        let meta = json!({
            "shareUrl": link::share_url(&code),
            "shortCode": code,
            "feedId": anon.id,
            "exportId": video.as_ref().map(|(ex, _)| ex.export_id.as_str()),
            "author": author,
            "description": description,
            "coverUrl": cover_url,
        });
        let pretty = serde_json::to_vec_pretty(&meta)
            .map_err(|e| FetchError::Internal(format!("serialize meta.json: {e}")))?;
        fs::write(out.join("meta.json"), pretty)
            .map_err(|e| FetchError::Internal(format!("write meta.json: {e}")))?;
        fs::write(out.join("feed.json"), &anon.raw)
            .map_err(|e| FetchError::Internal(format!("write feed.json: {e}")))?;
    }

    // 缺的 = 想要的 − 实际拿到的，和宿主重算的一样（协议 §3.1）
    let obtained = output.obtained();
    output.missing = wanted.difference(&obtained).copied().collect();
    output.completeness = if output.missing.is_empty() {
        Completeness::Full
    } else if obtained.is_disjoint(&wanted) {
        // 只要视频、又没登录元宝：说清楚要登录，而不是交回一个空快照
        if cookies.is_none() && wanted == BTreeSet::from([OutputKind::Video]) {
            return Err(FetchError::LoginRequired(
                "only media:video was wanted and there is no yuanbao login".into(),
            ));
        }
        Completeness::Link
    } else {
        Completeness::Partial
    };
    if !output.missing.is_empty() && !reasons.is_empty() {
        output.missing_reason = Some(reasons.join("；"));
    }
    Ok(output)
}

fn rel(path: &str) -> Result<RelPath, FetchError> {
    RelPath::try_from(path).map_err(FetchError::Internal)
}
