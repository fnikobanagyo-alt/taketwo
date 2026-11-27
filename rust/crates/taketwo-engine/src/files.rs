//! File answers shaped like Starlette's `FileResponse`: content type from the name as Python's `mimetypes`
//! guesses it, `content-length`, `last-modified`, `etag`, `accept-ranges` and single Range requests.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use std::path::Path;
use tower::ServiceExt;
use tower_http::services::ServeFile;

/// Python's `mimetypes.guess_type(name)[0] or "application/octet-stream"`, for the types TakeTwo serves.
/// Starlette adds `; charset=utf-8` to `text/` types.
pub fn guess_type(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let t = match ext.as_str() {
        "wav" => "audio/x-wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "json" => "application/json",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "txt" => "text/plain",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/vnd.microsoft.icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    };
    if t.starts_with("text/") { format!("{t}; charset=utf-8") } else { t.to_string() }
}

/// Starlette's Content-Disposition: `filename="..."` when `quote(name)` leaves it unchanged, else `filename*=utf-8''...`.
pub fn content_disposition(name: &str) -> String {
    // urllib.parse.quote(name) with the default safe="/": letters, digits, `_.-~` and `/` stay.
    let mut q = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~/".contains(&b) {
            q.push(b as char);
        } else {
            q.push_str(&format!("%{b:02X}"));
        }
    }
    if q != name { format!("attachment; filename*=utf-8''{q}") } else { format!("attachment; filename=\"{name}\"") }
}

/// Serve `path` (which must exist) as `content_type`, with `extra` headers on a 200 or 206 answer.
pub async fn serve(path: &Path, method: &Method, req: &HeaderMap, content_type: &str, extra: &[(HeaderName, String)]) -> Response {
    let mut r = Request::builder().method(method.clone()).uri("/");
    for h in [header::RANGE, header::IF_RANGE, header::IF_MODIFIED_SINCE, header::IF_UNMODIFIED_SINCE] {
        if let Some(v) = req.get(&h) {
            r = r.header(h, v);
        }
    }
    let r = r.body(Body::empty()).expect("valid request");
    let mut res = match ServeFile::new(path).oneshot(r).await {
        Ok(res) => res.map(Body::new),
        Err(e) => match e {},
    };
    if matches!(res.status(), StatusCode::OK | StatusCode::PARTIAL_CONTENT) {
        let h = res.headers_mut();
        if let Ok(v) = HeaderValue::from_str(content_type) {
            h.insert(header::CONTENT_TYPE, v);
        }
        if let Ok(m) = std::fs::metadata(path) {
            let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
            // A strong validator from mtime and size (Starlette hashes the same two with md5).
            if let Ok(v) = HeaderValue::from_str(&format!("\"{mtime:x}-{:x}\"", m.len())) {
                h.insert(header::ETAG, v);
            }
        }
        for (k, v) in extra {
            if let Ok(v) = HeaderValue::from_str(v) {
                h.insert(k.clone(), v);
            }
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starlette_disposition_forms() {
        assert_eq!(content_disposition("a.wav"), "attachment; filename=\"a.wav\"");
        assert_eq!(content_disposition("A b.wav"), "attachment; filename*=utf-8''A%20b.wav");
        assert_eq!(content_disposition("é/x.wav"), "attachment; filename*=utf-8''%C3%A9/x.wav");
        assert_eq!(guess_type("x.WAV"), "audio/x-wav");
        assert_eq!(guess_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(guess_type("noext"), "application/octet-stream");
    }
}
