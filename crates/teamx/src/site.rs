//! Teamx Cloud 官网（design/019）：**单页静态站点，内嵌进二进制**。
//!
//! - `teamx site` 一个进程同时服务：`/` 官网、`/assets/*` 静态资源、`/console` 平台控制台
//!   （控制台的 `/api/*` 保持绝对路径，因此 Login 是**站内相对路径**，同域共享 HTTPS 与 cookie）。
//! - 资源用 `include_str!` / `include_bytes!` 编译进二进制（SD-7/SD-16），
//!   部署时只需一个 `teamx` 可执行文件，无需 web 服务器。
//! - 站点为**零构建**单文件 HTML（内联 CSS/JS）+ 少量图片（SD-15/SD-16）。

/// 官网单页（含内联 CSS/JS 与中英词典）。
pub const INDEX_HTML: &str = include_str!("../../../site/index.html");

/// 内嵌静态资源：(文件名, 字节, Content-Type)。
pub const ASSETS: &[(&str, &[u8], &str)] = &[
    ("goal.jpg", include_bytes!("../../../site/assets/goal.jpg"), "image/jpeg"),
    ("members.jpg", include_bytes!("../../../site/assets/members.jpg"), "image/jpeg"),
    ("modes.jpg", include_bytes!("../../../site/assets/modes.jpg"), "image/jpeg"),
    ("cost.jpg", include_bytes!("../../../site/assets/cost.jpg"), "image/jpeg"),
];

/// 按文件名取内嵌资源；未命中返回 `None`（404）。
pub fn asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    ASSETS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, b, ct)| (*b, *ct))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_html_is_embedded_and_bilingual() {
        assert!(INDEX_HTML.contains("teamx"), "官网 HTML 应内嵌");
        // 关键内容块（design/019 SD-19..SD-29）
        for needle in [
            "I18N",                 // 中英词典
            "人 ×",                 // Hero 公式
            "team with AI = win",   // 末屏收束（SD-23）
            "三个臭皮匠",            // 谚语（中文正字，SD-28）
            "三人寄れば文殊の知恵",  // 日文谚语
            "Many hands make light work",
            "brew install caishu97/tap/teamx", // 安装（SD-9）
            "/console",             // Login 站内相对路径（SD-11）
        ] {
            assert!(INDEX_HTML.contains(needle), "官网缺少内容: {needle}");
        }
        assert!(!INDEX_HTML.contains("Windows"), "SD-9：安装区不提 Windows");
    }

    #[test]
    fn assets_are_embedded_and_typed() {
        for name in ["goal.jpg", "members.jpg", "modes.jpg", "cost.jpg"] {
            let (bytes, ct) = asset(name).unwrap_or_else(|| panic!("缺少资源 {name}"));
            assert!(bytes.len() > 1000, "{name} 太小，疑似占位");
            assert_eq!(ct, "image/jpeg");
        }
        assert!(asset("nope.jpg").is_none(), "未命中应返回 None");
        assert!(asset("../index.html").is_none(), "路径穿越不得命中");
    }
}
