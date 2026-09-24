use crate::{
    results::{ResultError, ResultViewMetadata},
    state::AppState,
};
use ammonia::{Builder, UrlRelative};
use axum::{
    extract::{Path, State},
    http::{
        HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS},
    },
    response::{IntoResponse, Response},
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, html};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, SemaphorePermit};

static READS: Semaphore = Semaphore::const_new(4);
static RATE: Mutex<(Option<Instant>, u32)> = Mutex::new((None, 0));
const CSS: &str = include_str!("result_assets/results-v1.css");
const JS: &str = include_str!("result_assets/results-v1.js");

fn admission() -> Option<SemaphorePermit<'static>> {
    let mut rate = RATE.lock().unwrap_or_else(|e| e.into_inner());
    if rate
        .0
        .is_none_or(|at| at.elapsed() >= Duration::from_secs(1))
    {
        *rate = (Some(Instant::now()), 0);
    }
    if rate.1 >= 32 {
        return None;
    }
    rate.1 += 1;
    READS.try_acquire().ok()
}
fn busy() -> Response {
    let mut response = public_error(ResultError::Unavailable);
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("1"));
    response
}
pub async fn page(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let permit = match admission() {
        Some(permit) => permit,
        None => return busy(),
    };
    match state.results.public_body(&token).await {
        Ok((body, metadata)) => {
            // Work stays bounded even if the HTTP timeout drops this future.
            match tokio::task::spawn_blocking(move || {
                let _permit = permit;
                render(&body, &metadata, &format!("/r/{token}/raw"))
            })
            .await
            {
                Ok(page) => secured(StatusCode::OK, "text/html; charset=utf-8", page),
                Err(_) => public_error(ResultError::Unavailable),
            }
        }
        Err(error) => public_error(error),
    }
}
pub async fn raw(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let _permit = match admission() {
        Some(permit) => permit,
        None => return busy(),
    };
    match state.results.public_body(&token).await {
        Ok((body, _)) => secured(StatusCode::OK, "text/plain; charset=utf-8", body),
        Err(error) => public_error(error),
    }
}
pub async fn asset(Path(asset): Path<String>) -> Response {
    let (content_type, body) = match asset.as_str() {
        "results-v1.css" => ("text/css; charset=utf-8", CSS),
        "results-v1.js" => ("text/javascript; charset=utf-8", JS),
        _ => {
            return secured(
                StatusCode::NOT_FOUND,
                "text/plain; charset=utf-8",
                "not found".to_owned(),
            );
        }
    };
    let mut response = (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response();
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}
fn safe_link(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && url.has_host()
            && url.username().is_empty()
            && url.password().is_none()
    })
}
fn markdown(body: &str) -> String {
    let mut links = Vec::new();
    let parser = Parser::new_ext(
        body,
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
    )
    .filter_map(|event| {
        Some(match event {
            Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
            Event::Start(Tag::Image { .. }) => Event::Text("[图片：".into()),
            Event::End(TagEnd::Image) => Event::Text("]（未加载外部图片）".into()),
            Event::Start(tag @ Tag::Link { .. }) => {
                let Tag::Link { ref dest_url, .. } = tag else {
                    unreachable!()
                };
                let allowed = safe_link(dest_url);
                links.push(allowed);
                if !allowed {
                    return None;
                }
                Event::Start(tag)
            }
            Event::End(TagEnd::Link) => {
                if !links.pop().unwrap_or(false) {
                    return None;
                }
                Event::End(TagEnd::Link)
            }
            Event::TaskListMarker(done) => Event::Text(if done { "[x] " } else { "[ ] " }.into()),
            other => other,
        })
    });
    let mut generated = String::new();
    html::push_html(&mut generated, parser);
    // All untrusted HTML transformations precede this final explicit allowlist.
    Builder::empty()
        .tags(HashSet::from([
            "p",
            "br",
            "hr",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "blockquote",
            "ul",
            "ol",
            "li",
            "pre",
            "code",
            "em",
            "strong",
            "del",
            "a",
            "table",
            "thead",
            "tbody",
            "tr",
            "th",
            "td",
        ]))
        .tag_attributes(HashMap::from([
            ("a", HashSet::from(["href", "title"])),
            ("ol", HashSet::from(["start"])),
        ]))
        .url_relative(UrlRelative::Deny)
        .url_schemes(HashSet::from(["http", "https"]))
        .link_rel(Some("noopener noreferrer"))
        .clean(&generated)
        .to_string()
}
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
pub(crate) fn render(body: &str, metadata: &ResultViewMetadata, raw_path: &str) -> String {
    let safe = markdown(body);
    let title = escape(&metadata.title);
    let expiry = metadata.page_expires_at;
    let expiry_text = display_time(expiry);
    let completed = metadata
        .completed_at
        .map(|at| {
            format!(
                "<span>结束观察 <time data-ms=\"{at}\">{}</time></span>",
                display_time(at)
            )
        })
        .unwrap_or_default();
    let duration = metadata
        .duration_ms
        .map(|duration| format!("<span>耗时 {} 秒</span>", duration as f64 / 1000.0))
        .unwrap_or_default();
    let raw_path = escape(raw_path);
    let empty = if body.is_empty() {
        "<p class=\"empty\">本轮最终回答为空。</p>"
    } else {
        ""
    };
    format!(
        r##"<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title} · PromptDock</title><link rel="stylesheet" href="/result-assets/results-v1.css"><script src="/result-assets/results-v1.js" defer></script></head><body><main><header><a class="brand" href="#result">PromptDock</a><h1>{title}</h1><p>Codex 本轮输出已结束</p><div class="metadata">{completed}{duration}<span>有效期至 <time data-ms="{expiry}">{expiry_text}</time></span></div></header><nav aria-label="阅读操作"><button id="copy-all" hidden type="button">复制全文</button><button id="toggle-raw" hidden type="button" aria-pressed="false">显示原文</button><a id="raw-link" href="{raw_path}">打开原文</a></nav><p id="action-status" role="status" aria-live="polite"></p><article id="result">{empty}{safe}</article><section id="raw-panel" hidden><label for="raw-text">完整原文（可选择复制）</label><textarea id="raw-text" readonly spellcheck="false"></textarea></section><footer>此链接仅供阅读。有效期内，持有链接的人可以查看结果。</footer></main></body></html>"##
    )
}
fn display_time(milliseconds: i64) -> String {
    chrono::DateTime::from_timestamp_millis(milliseconds)
        .map(|date| date.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "时间不可用".to_owned())
}
fn public_error(error: ResultError) -> Response {
    let (status, message) = match error {
        ResultError::Inaccessible | ResultError::NotFound | ResultError::Validation => {
            (StatusCode::NOT_FOUND, "结果不可访问")
        }
        _ => (StatusCode::SERVICE_UNAVAILABLE, "服务暂不可用，请稍后重试"),
    };
    secured(
        status,
        "text/html; charset=utf-8",
        format!(
            "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{message}</title><link rel=\"stylesheet\" href=\"/result-assets/results-v1.css\"><main><h1>{message}</h1><p>请向发送者确认链接，或稍后重新打开。</p></main></html>"
        ),
    )
}
fn secured(status: StatusCode, content_type: &str, body: String) -> Response {
    let mut response = (
        status,
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "no-store"),
            (REFERRER_POLICY, "no-referrer"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response();
    response.headers_mut().insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow, noarchive"),
    );
    response.headers_mut().insert("content-security-policy", HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; img-src 'none'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"));
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_document_keeps_reading_structure_and_can_export_visual_fixture() {
        let body = format!(
            "## 验证结果\n\n完整回答包含中文、🙂 和长路径 C:\\{}\n\n| 检查项 | 说明 |\n| --- | --- |\n| 原文 | CRLF、代码和尾部空白保持原样 |\n\n```rust\r\nfn main() {{ println!(\"{}\"); }}\r\n```\n\n<script>bad()</script>\n\n{}",
            "project\\".repeat(24),
            "long-code-".repeat(30),
            "这是保留原文的阅读段落。\n\n".repeat(10)
        );
        let metadata = ResultViewMetadata {
            title: "Codex 最终回答".into(),
            page_expires_at: 1800604800000,
            completed_at: Some(1800000000000),
            duration_ms: Some(12345),
            source_hash: "a".repeat(64),
        };
        let page = render(&body, &metadata, "/r/visual-fixture/raw");
        assert!(page.contains("<table>"));
        assert!(page.contains("<pre><code>"));
        assert!(page.contains("UTC"));
        assert!(!page.contains("<script>bad()"));
        if let Some(directory) = std::env::var_os("PROMPTDOCK_VIEWER_QA_DIR") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("index.html"), page).unwrap();
            std::fs::write(directory.join("raw.txt"), body).unwrap();
            std::fs::write(directory.join("results-v1.css"), CSS).unwrap();
            std::fs::write(directory.join("results-v1.js"), JS).unwrap();
        }
    }
    #[test]
    fn html_is_visible_text_and_links_media_cannot_execute() {
        let result = markdown(
            "<script>alert(1)</script>\n\n<svg onload=alert(2)></svg>\n\n[x](javascript:alert) [admin](/admin) [safe](https://example.com) ![secret](https://example.com/pixel)\n\n```x\"onmouseover=alert(3)\n</textarea></script>\n```\n",
        );
        assert!(result.contains("&lt;script&gt;"));
        for forbidden in [
            "<script",
            "<svg",
            "<img",
            "href=\"javascript:",
            "href=\"/admin",
            "onmouseover=\"",
        ] {
            assert!(!result.contains(forbidden), "{forbidden}");
        }
        assert!(result.contains("href=\"https://example.com\""));
        assert!(result.contains("noopener noreferrer"));
        assert!(result.contains("secret"));
    }
    #[test]
    fn template_escapes_title_and_does_not_embed_raw_text_in_script() {
        let metadata = ResultViewMetadata {
            title: "</title><script>bad()</script>".into(),
            page_expires_at: 1800604800000,
            completed_at: None,
            duration_ms: None,
            source_hash: "a".repeat(64),
        };
        let page = render("</textarea>\r\n中文🙂", &metadata, "/r/test/raw");
        assert!(!page.contains("<script>bad()"));
        assert!(page.contains("&lt;/textarea&gt;"));
        assert!(page.contains("readonly spellcheck"));
        assert!(page.contains("/r/test/raw"));
    }
}
