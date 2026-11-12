//! 取一条（协议 §3.6、§3.7），两种方法（§2.3.1）：
//!
//! - `feed-info`：不带账号读分享页的接口，交作者、描述、封面和发布时间。这一条的视频它取不到，
//!   总在 `missing` 里写 `media:video`，宿主再找声明了视频的方法。
//! - `yuanbao`：用你的腾讯元宝登录换临时凭证，只交视频文件。

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
use crate::{Config, FetchError, YUANBAO_SITE, download, feed, identity, link, yuanbao};

/// `feed-info` 的输出，和清单里这种方法的 `outputs` 一致。
pub const FEED_OUTPUTS: [OutputKind; 3] = [OutputKind::Text, OutputKind::Images, OutputKind::Meta];
/// `yuanbao` 的输出。
pub const YUANBAO_OUTPUTS: [OutputKind; 1] = [OutputKind::Video];

const VIDEO_FILE: &str = "media/video.mp4";

/// `feed-info` 交不出视频时的 `missingReason`。
pub const NO_VIDEO_REASON: &str =
    "分享页没有给出视频地址；在设置里登录腾讯元宝后，可以用元宝账号取到视频";

fn wanted(
    want: Option<&[OutputKind]>,
    outputs: &[OutputKind],
) -> Result<BTreeSet<OutputKind>, FetchError> {
    let wanted: BTreeSet<OutputKind> = want.map_or_else(
        || outputs.iter().copied().collect(),
        |w| w.iter().copied().collect(),
    );
    if let Some(other) = wanted.iter().find(|w| !outputs.contains(w)) {
        return Err(FetchError::InvalidArgs(format!("cannot give `{other}`")));
    }
    Ok(wanted)
}

fn short_code(input: &str) -> Result<String, FetchError> {
    link::short_code(input)
        .ok_or_else(|| FetchError::UnsupportedInput(input.chars().take(200).collect()))
}

fn empty_output(
    code: &str,
    access: Access,
    site: Option<&str>,
    status: Option<u16>,
) -> FetchOutput {
    FetchOutput {
        identity: Some(identity(code)),
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
            fetched_at: Rfc3339::from_timestamp(jiff::Timestamp::now()),
            access,
            site: site.map(str::to_owned),
            upstream: None,
            status,
        },
        credentials_update: None,
    }
}

/// `feed-info`：不带任何 cookie。
pub fn feed_info(
    cfg: &Config,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<FetchOutput, FetchError> {
    let code = short_code(input)?;
    let wanted = wanted(want, &FEED_OUTPUTS)?;
    let anon = feed::anonymous(cfg, &code)?;
    let mut reasons: Vec<String> = Vec::new();
    let fields = &anon.fields;
    let description = fields.description.clone().filter(|d| !d.trim().is_empty());
    let cover_url = fields.cover_url.clone().filter(|u| !u.trim().is_empty());
    let author = fields.author.clone();

    let mut output = empty_output(&code, Access::PublicWeb, None, Some(anon.status));
    output.published_at = published_at(fields);
    if wanted.contains(&OutputKind::Text) {
        output.text = description.clone();
        output.text_format = output.text.as_ref().map(|_| TextFormat::Plain);
    }
    if wanted.contains(&OutputKind::Meta) {
        output.author = author.clone().map(|name| Author {
            name,
            platform_id: None,
            url: None,
        });
    }
    if wanted.contains(&OutputKind::Images)
        && let Some(url) = &cover_url
    {
        match download::cover(cfg, url) {
            Ok((bytes, ext)) => {
                let name = format!("images/cover.{ext}");
                fs::create_dir_all(out.join("images"))
                    .map_err(|e| FetchError::Internal(format!("create images/: {e}")))?;
                fs::write(out.join(&name), bytes)
                    .map_err(|e| FetchError::Internal(format!("write {name}: {e}")))?;
                output.images.push(rel(&name)?);
            }
            Err(why) => reasons.push(why),
        }
    }
    if wanted.contains(&OutputKind::Meta) {
        // 协议的输出没有短码、描述、封面地址这几栏，写进 meta.json，随快照留在 raw/。
        let meta = json!({
            "shareUrl": link::share_url(&code),
            "shortCode": code,
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

    // 缺的 = 这一条有、想要、又没交来的（协议 §3.7）。没有描述就没有文字，没有封面地址就没有图片。
    // 这个平台的每一条都是视频：没给 `--want`（什么都想要）时视频总算缺，宿主去找能取视频的方法。
    let mut absent: BTreeSet<OutputKind> = BTreeSet::new();
    if description.is_none() {
        absent.insert(OutputKind::Text);
    }
    if cover_url.is_none() {
        absent.insert(OutputKind::Images);
    }
    let obtained = output.obtained();
    output.missing = wanted
        .iter()
        .filter(|kind| !absent.contains(kind) && !obtained.contains(kind))
        .copied()
        .collect();
    if want.is_none() {
        output.missing.push(OutputKind::Video);
        reasons.insert(0, NO_VIDEO_REASON.into());
    }
    output.completeness = if output.missing.is_empty() {
        Completeness::Full
    } else if obtained.is_disjoint(&wanted) {
        Completeness::Link
    } else {
        Completeness::Partial
    };
    if !output.missing.is_empty() && !reasons.is_empty() {
        output.missing_reason = Some(reasons.join("；"));
    }
    Ok(output)
}

/// `yuanbao`：只交视频。宿主没交元宝的登录就回 `login-required`。
pub fn yuanbao(
    cfg: &Config,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
    cookies: Option<&YuanbaoCookies>,
) -> Result<FetchOutput, FetchError> {
    let code = short_code(input)?;
    wanted(want, &YUANBAO_OUTPUTS)?;
    let cookies = cookies.ok_or_else(|| {
        FetchError::LoginRequired("there is no yuanbao login for this call".into())
    })?;
    let exchange =
        yuanbao::exchange(cfg, &code, cookies).map_err(yuanbao::VideoError::into_fetch_error)?;
    let video =
        yuanbao::video_feed(cfg, &exchange).map_err(yuanbao::VideoError::into_fetch_error)?;
    download::video(cfg, &video.video_url, &out.join(VIDEO_FILE))
        .map_err(FetchError::UpstreamChanged)?;
    let mut output = empty_output(&code, Access::SessionApi, Some(YUANBAO_SITE), None);
    output.published_at = published_at(&video.fields);
    output.media.push(Media {
        path: rel(VIDEO_FILE)?,
        kind: MediaKind::Video,
        mime: Some("video/mp4".into()),
        lang: None,
        duration_sec: None,
        group: None,
    });
    Ok(output)
}

/// 发布时间：回答里的 `createtime`。
fn published_at(fields: &FeedFields) -> Option<Rfc3339> {
    let secs = fields.created_at?;
    jiff::Timestamp::from_second(secs)
        .ok()
        .map(Rfc3339::from_timestamp)
}

fn rel(path: &str) -> Result<RelPath, FetchError> {
    RelPath::try_from(path).map_err(FetchError::Internal)
}
