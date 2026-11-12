//! 这个插件能报的失败，和它们在协议里的错误码（协议 §3.3）。

use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode};

use crate::YUANBAO_SITE;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("not a WeChat Channels share link: {0}")]
    UnsupportedInput(String),
    #[error("{0}")]
    InvalidArgs(String),
    /// 分享页的接口说这一条看不到了；文字取自它的回答，只进日志。
    #[error("feed unavailable: {0}")]
    NotFound(String),
    /// 元宝的登录没有或过期了：401，或元宝错误码 20000。
    #[error("yuanbao login rejected: {0}")]
    LoginRequired(String),
    #[error("rate limited by {host}")]
    RateLimited {
        host: &'static str,
        retry_after: Option<u64>,
    },
    /// 回答的形状不是预期的：不是 JSON、缺字段、字段类型不对。
    #[error("upstream changed: {0}")]
    UpstreamChanged(String),
    #[error("network: {0}")]
    Network(String),
    #[error("{0}")]
    Internal(String),
    /// 平台回了，但不是能用的回答：带着回答的开头，宿主记进失败日志。
    #[error("{error}")]
    Answered {
        error: Box<FetchError>,
        answer: String,
    },
}

/// 留给失败日志的回答开头最多多少字节。
const ANSWER_BYTES: usize = 4096;

impl FetchError {
    /// 带上平台回答的开头。
    pub fn answered(self, raw: &[u8]) -> FetchError {
        let end = raw.len().min(ANSWER_BYTES);
        FetchError::Answered {
            error: Box::new(self),
            answer: String::from_utf8_lossy(&raw[..end]).into_owned(),
        }
    }

    pub fn into_failure(self) -> CallFailure {
        let message = self.to_string();
        match self {
            FetchError::Answered { error, answer } => error.into_failure().with_detail(&answer),
            FetchError::UnsupportedInput(_) => {
                CallFailure::new(ErrorCode::UnsupportedInput, message)
            }
            FetchError::InvalidArgs(_) => CallFailure::new(ErrorCode::InvalidArgs, message),
            FetchError::NotFound(_) => CallFailure {
                hint: Some("这条视频号内容在分享页上已经看不到了".into()),
                ..CallFailure::new(ErrorCode::NotFound, message)
            },
            FetchError::LoginRequired(_) => CallFailure {
                hint: Some(
                    "取视频要用腾讯元宝的登录：请在设置 › 平台与获取方式里登录腾讯元宝；登录过的，可能已经过期".into(),
                ),
                site: Some(YUANBAO_SITE.into()),
                ..CallFailure::new(ErrorCode::LoginRequired, message)
            },
            FetchError::RateLimited { retry_after, .. } => CallFailure {
                retry_after,
                ..CallFailure::new(ErrorCode::RateLimited, message)
            },
            FetchError::UpstreamChanged(_) => CallFailure::new(ErrorCode::UpstreamChanged, message),
            FetchError::Network(_) => CallFailure::new(ErrorCode::Network, message),
            FetchError::Internal(_) => CallFailure::new(ErrorCode::Internal, message),
        }
    }
}
