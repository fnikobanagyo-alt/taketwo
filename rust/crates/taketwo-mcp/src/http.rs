//! The one seam under the tools: an HTTP request. `Ureq` is the real one; unit tests feed canned replies.

use std::io::Read;
use std::time::Duration;

/// Python's `HEADERS` in mcp/taketwo_mcp.py.
pub const HEADERS: [(&str, &str); 3] =
    [("Content-Type", "application/json"), ("X-TakeTwo-Client", "mcp"), ("X-VoiceForge-Client", "mcp")];

/// An answer, error statuses included: `status`, the status line's reason phrase and the body as a stream.
pub struct Reply {
    pub status: u16,
    pub reason: String,
    pub body: Box<dyn Read + Send>,
}

pub trait Transport: Send + Sync {
    /// One request to a full URL. `Err` only when no HTTP answer came (refused, timed out, broken).
    fn send(&self, method: &str, url: &str, body: Option<Vec<u8>>, timeout: Duration) -> std::io::Result<Reply>;
}

/// ureq 2, timeouts per connect/read/write as urllib's socket timeout. No proxy, no TLS (the engine is local).
pub struct Ureq;

impl Transport for Ureq {
    fn send(&self, method: &str, url: &str, body: Option<Vec<u8>>, timeout: Duration) -> std::io::Result<Reply> {
        let agent = ureq::AgentBuilder::new().timeout_connect(timeout).timeout_read(timeout).timeout_write(timeout).build();
        let mut r = agent.request(method, url);
        for (k, v) in HEADERS {
            r = r.set(k, v);
        }
        let answer = match body {
            Some(b) => r.send_bytes(&b),
            None => r.call(),
        };
        let resp = match answer {
            Ok(resp) | Err(ureq::Error::Status(_, resp)) => resp,
            Err(e) => return Err(std::io::Error::other(e.to_string())),
        };
        Ok(Reply { status: resp.status(), reason: resp.status_text().to_string(), body: Box::new(resp.into_reader()) })
    }
}
