# usuyo-fetch/wechat-channels

微信视频号的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按分享链接取回单条视频的作者、描述、封面和视频。

| 方法 | 清单里的 id | 取得 | 账号 | 层级 | 现状 |
|---|---|---|---|---|---|
| 分享页接口 | `feed-info` | 作者、描述、封面、发布时间；取不到视频 | 不使用 | `public-web` | 已实现；2026-11-12 对真实平台跑过 |
| 腾讯元宝换视频 | `yuanbao` | 视频 MP4 | 登录进 usuyo（元宝） | `session-api` | 已实现，没对真实平台跑过 |
| 网页版的浏览器脚本 | `userscript` | 链接 | 只在使用者的浏览器内 | `browser-session` | 未实现 |
| iPhone 镜像 | `iphone` | 链接 | 只在手机的 App 内 | `vision-ui` | 未实现 |
| 电脑微信 | `mac` | 链接 | 只在电脑微信内 | `vision-ui` | 未实现 |
| 安卓手机 | `android` | 链接 | 只在手机的 App 内 | `vision-ui` | 未实现 |
| 录下口播 | `audio` | 口播 | 只在手机的 App 内 | `vision-ui` | 未实现 |

请求与解析移植自 [ParseHub](https://github.com/z-mio/ParseHub)（MIT，`wxchannels.py`，许可证原文在 `src/yuanbao.rs` 顶部）。测试：`cargo test`。依赖同级目录下 usuyo 仓库的 `usuyo-plugin-kit`。许可证：MIT。
