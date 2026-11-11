//! 认链接：手机分享出来的两种形状，都取出短码。不发请求。
//!
//! - `https://weixin.qq.com/sph/<短码>`：「分享 → 复制链接」得到的
//! - `https://channels.weixin.qq.com/finder-preview/pages/sph?id=<短码>`：在浏览器里打开上面那条后落到的页面
//!
//! 两种形状照 ParseHub `wxchannels.py` 的 `_ID_RE`（MIT，见 `yuanbao.rs` 顶部的许可证）。

/// 短码最长多少：分享链接里的短码只有十来个字符，留足余量，挡住拼进来的长串。
const MAX_CODE: usize = 64;

/// 取出短码；不是这两种链接就是 `None`。
pub fn short_code(input: &str) -> Option<String> {
    let url = url::Url::parse(input.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let code = match url.host_str()? {
        "weixin.qq.com" => {
            let rest = url.path().strip_prefix("/sph/")?;
            rest.strip_suffix('/').unwrap_or(rest).to_owned()
        }
        "channels.weixin.qq.com" if url.path() == "/finder-preview/pages/sph" => url
            .query_pairs()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.into_owned())?,
        _ => return None,
    };
    is_code(&code).then_some(code)
}

fn is_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// 规范链接：用户分享出来的那种，只用于去重和显示。
pub fn share_url(code: &str) -> String {
    format!("https://weixin.qq.com/sph/{code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes() {
        let table = [
            ("https://weixin.qq.com/sph/AbC123_x", Some("AbC123_x")),
            ("https://weixin.qq.com/sph/AbC123/", Some("AbC123")),
            (
                "http://weixin.qq.com/sph/AbC123?from=timeline#x",
                Some("AbC123"),
            ),
            (
                "https://channels.weixin.qq.com/finder-preview/pages/sph?id=AbC123&scene=1",
                Some("AbC123"),
            ),
            (
                "https://channels.weixin.qq.com/finder-preview/pages/sph?scene=1&id=AbC123",
                Some("AbC123"),
            ),
            ("  https://weixin.qq.com/sph/AbC123\n", Some("AbC123")),
            ("https://weixin.qq.com/sph/", None),
            ("https://weixin.qq.com/sph/a/b", None),
            ("https://weixin.qq.com/sph/a%2Fb", None),
            ("https://weixin.qq.com/s/AbC123", None),
            ("https://evil.weixin.qq.com.example/sph/AbC123", None),
            (
                "https://channels.weixin.qq.com/finder-preview/pages/feed?id=AbC123",
                None,
            ),
            (
                "https://channels.weixin.qq.com/finder-preview/pages/sph",
                None,
            ),
            ("ftp://weixin.qq.com/sph/AbC123", None),
            ("not a url", None),
        ];
        for (input, want) in table {
            assert_eq!(short_code(input).as_deref(), want, "{input}");
        }
        let long = format!("https://weixin.qq.com/sph/{}", "a".repeat(MAX_CODE + 1));
        assert_eq!(short_code(&long), None);
    }
}
