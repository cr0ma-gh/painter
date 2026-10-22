//! A small HTTP/1.1 client for eSCL: plain HTTP on the local network, one
//! request per connection (`Connection: close`), bodies by `Content-Length`,
//! chunked transfer or the connection closing.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A parsed `http://host[:port]/path` URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Url {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(url: &str) -> Option<Url> {
        let rest = url.strip_prefix("http://")?;
        let (authority, path) = rest.split_once('/').map_or((rest, "/".to_string()), |(a, p)| (a, format!("/{p}")));
        // [IPv6]:port, host:port or host.
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6.split_once(']')?;
            (h.to_string(), after.strip_prefix(':').map_or(Some(80), |p| p.parse().ok())?)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().ok()?),
                None => (authority.to_string(), 80),
            }
        };
        (!host.is_empty()).then_some(Url { host, port, path })
    }

    /// `location` (absolute, or a path on this server) resolved against this URL.
    pub fn join(&self, location: &str) -> Option<Url> {
        if location.starts_with("http://") {
            Url::parse(location)
        } else if location.starts_with('/') {
            Some(Url { path: location.to_string(), ..self.clone() })
        } else {
            let base = self.path.rsplit_once('/').map_or("", |(b, _)| b);
            Some(Url { path: format!("{base}/{location}"), ..self.clone() })
        }
    }

    fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == 80 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// Sends one request and reads the whole response.
pub fn request(method: &str, url: &Url, body: Option<(&str, &[u8])>, timeout: Duration) -> Result<Response, String> {
    let addr =
        (url.host.as_str(), url.port).to_socket_addrs().map_err(|e| format!("{}: {e}", url.host))?.next().ok_or("no address")?;
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| format!("{}: {e}", url.host))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: Painter\r\nAccept: */*\r\nConnection: close\r\n",
        url.path,
        url.host_header()
    );
    if let Some((kind, data)) = body {
        head += &format!("Content-Type: {kind}\r\nContent-Length: {}\r\n", data.len());
    } else if method == "POST" || method == "PUT" {
        head += "Content-Length: 0\r\n";
    }
    head += "\r\n";
    stream.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
    if let Some((_, data)) = body {
        stream.write_all(data).map_err(|e| e.to_string())?;
    }
    // Read until the response is complete: some devices (Brother) keep the
    // connection open after the last chunk despite `Connection: close`.
    let mut raw = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if is_complete(&raw) {
                    break;
                }
            }
            Err(e) if raw.is_empty() || !is_complete(&raw) => return Err(e.to_string()),
            Err(_) => break,
        }
    }
    parse_response(&raw)
}

/// Whether `raw` holds a whole response (by its length or its last chunk);
/// without either, only the connection closing ends it.
fn is_complete(raw: &[u8]) -> bool {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else { return false };
    let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
    let body = &raw[end + 4..];
    let header = |name: &str| head.lines().find_map(|l| l.strip_prefix(name).map(|v| v.trim_start_matches(':').trim().to_string()));
    if header("transfer-encoding").is_some_and(|t| t == "chunked") {
        return dechunk(body).is_ok();
    }
    if let Some(len) = header("content-length").and_then(|l| l.parse::<usize>().ok()) {
        return body.len() >= len;
    }
    // No body expected.
    head.split_whitespace().nth(1).is_some_and(|s| s == "204" || s == "304" || s.starts_with('1'))
}

/// Parses a complete HTTP/1.1 response.
pub fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no response")?;
    let head = String::from_utf8_lossy(&raw[..end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or("no status line")?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line {status_line:?}"))?;
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    let rest = &raw[end + 4..];
    let response = Response { status, headers, body: Vec::new() };
    let body = if response.header("Transfer-Encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        dechunk(rest)?
    } else if let Some(len) = response.header("Content-Length").and_then(|l| l.parse::<usize>().ok()) {
        rest.get(..len).ok_or("response shorter than its length")?.to_vec()
    } else {
        rest.to_vec()
    };
    Ok(Response { body, ..response })
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(data.get(..size).ok_or("truncated chunk")?);
        data = data.get(size + 2..).unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse_and_join() {
        let u = Url::parse("http://192.168.1.20:8080/eSCL").unwrap();
        assert_eq!((u.host.as_str(), u.port, u.path.as_str()), ("192.168.1.20", 8080, "/eSCL"));
        let v = Url::parse("http://scanner.local").unwrap();
        assert_eq!((v.port, v.path.as_str()), (80, "/"));
        let w = Url::parse("http://[fe80::1]:631/x").unwrap();
        assert_eq!((w.host.as_str(), w.port, w.host_header()), ("fe80::1", 631, "[fe80::1]:631".to_string()));
        assert_eq!(u.join("/eSCL/ScanJobs/7").unwrap().path, "/eSCL/ScanJobs/7");
        assert_eq!(u.join("http://10.0.0.2/eSCL/ScanJobs/8").unwrap().host, "10.0.0.2");
        assert!(Url::parse("https://x").is_none());
    }

    #[test]
    fn reads_length_chunked_and_closed_bodies() {
        let r = parse_response(b"HTTP/1.1 201 Created\r\nLocation: /eSCL/ScanJobs/1\r\nContent-Length: 2\r\n\r\nok").unwrap();
        assert_eq!((r.status, r.header("location"), r.body.as_slice()), (201, Some("/eSCL/ScanJobs/1"), &b"ok"[..]));
        let c =
            parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap();
        assert_eq!(c.body, b"Wikipedia");
        let d = parse_response(b"HTTP/1.0 503 Service Unavailable\r\n\r\nbusy").unwrap();
        assert_eq!((d.status, d.body.as_slice()), (503, &b"busy"[..]));
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn knows_when_a_response_is_whole() {
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab"));
        assert!(is_complete(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd"));
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n"));
        assert!(is_complete(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n0\r\n\r\n"));
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\n\r\nuntil close"));
        assert!(!is_complete(b"HTTP/1.1 200 OK\r\nContent-"));
    }
}
