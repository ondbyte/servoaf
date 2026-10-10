// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! MCP Streamable HTTP transport (JSON responses, no server-initiated SSE).
//!
//! Requests are accepted on a background thread and handed to the main thread
//! as [`McpRequest`]s; each HTTP connection waits on its own thread for the
//! reply, so slow tool calls don't block other clients.

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Sender, bounded};
use tiny_http::{Header, Method, Request, Response, Server};

use super::protocol::is_initialize_request;

pub const MCP_PATH: &str = "/mcp";
const SESSION_HEADER: &str = "Mcp-Session-Id";
const MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct HttpOptions {
    pub host: String,
    pub port: u16,
    /// If set, requests must carry `Authorization: Bearer <token>`.
    pub token: Option<String>,
}

/// One JSON-RPC message for the main thread. Send `None` back for "no
/// response body" (HTTP 202).
pub struct McpRequest {
    pub body: String,
    pub reply: Sender<Option<String>>,
}

pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Binds the listening socket and starts serving. Returns the bound address.
pub fn start(options: HttpOptions, requests: Sender<McpRequest>, wake: Wake) -> Result<SocketAddr, String> {
    let address = format!("{}:{}", options.host, options.port);
    let server = Server::http(&address).map_err(|error| format!("cannot listen on {address}: {error}"))?;
    let bound = server.server_addr().to_ip().ok_or_else(|| "server is not on an IP socket".to_owned())?;
    let options = Arc::new(options);
    thread::Builder::new()
        .name("mcp-http".into())
        .spawn(move || {
            for request in server.incoming_requests() {
                let (options, requests, wake) = (options.clone(), requests.clone(), wake.clone());
                let _ = thread::Builder::new()
                    .name("mcp-conn".into())
                    .spawn(move || handle(request, &options, &requests, &wake));
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(bound)
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}

fn make_header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn host_of(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        // [::1]:9223
        return rest.split(']').next().unwrap_or_default();
    }
    authority.rsplit_once(':').map_or(authority, |(host, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) { host } else { authority }
    })
}

/// DNS-rebinding protection: only loopback names or IP literals may be used
/// to reach the server.
pub fn is_allowed_host(authority: &str) -> bool {
    let host = host_of(authority);
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok()
}

/// Browsers send `Origin`; only allow pages served from loopback/IP origins.
pub fn is_allowed_origin(origin: &str) -> bool {
    if origin == "null" {
        return false;
    }
    match url::Url::parse(origin) {
        Ok(url) => url.host().is_some_and(|host| match host {
            url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
            _ => true,
        }),
        Err(_) => false,
    }
}

fn text_response(status: u16, body: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    Response::from_string(body)
        .with_status_code(status)
        .with_header(make_header("Content-Type", "text/plain"))
}

fn handle(mut request: Request, options: &HttpOptions, requests: &Sender<McpRequest>, wake: &Wake) {
    let host_ok = header(&request, "Host").is_none_or(is_allowed_host);
    let origin_ok = header(&request, "Origin").is_none_or(is_allowed_origin);
    if !host_ok || !origin_ok {
        let _ = request.respond(text_response(403, "Forbidden origin/host"));
        return;
    }
    if let Some(token) = &options.token
        && header(&request, "Authorization") != Some(&format!("Bearer {token}"))
    {
        let response =
            text_response(401, "Unauthorized").with_header(make_header("WWW-Authenticate", "Bearer"));
        let _ = request.respond(response);
        return;
    }

    let path = request.url().split('?').next().unwrap_or_default().to_owned();
    let method = request.method().clone();
    if path == "/" && method == Method::Get {
        let response = Response::from_string(r#"{"name":"servoaf","mcp_endpoint":"/mcp"}"#)
            .with_header(make_header("Content-Type", "application/json"));
        let _ = request.respond(response);
        return;
    }
    if path != MCP_PATH {
        let _ = request.respond(text_response(404, "Not found"));
        return;
    }
    match method {
        Method::Post => {},
        // Session termination; sessions carry no server-side state.
        Method::Delete => {
            let _ = request.respond(text_response(200, ""));
            return;
        },
        // No server-initiated SSE stream is offered (allowed by the spec).
        _ => {
            let response = text_response(405, "").with_header(make_header("Allow", "POST, DELETE"));
            let _ = request.respond(response);
            return;
        },
    }

    let mut body = String::new();
    if request.as_reader().take(MAX_BODY_BYTES + 1).read_to_string(&mut body).is_err()
        || body.len() as u64 > MAX_BODY_BYTES
    {
        let _ = request.respond(text_response(413, "Body too large or not UTF-8"));
        return;
    }
    let is_initialize = is_initialize_request(&body);
    let (reply_sender, reply_receiver) = bounded(1);
    if requests.send(McpRequest { body, reply: reply_sender }).is_err() {
        let _ = request.respond(text_response(503, "Browser is shutting down"));
        return;
    }
    wake();
    let reply = match reply_receiver.recv() {
        Ok(reply) => reply,
        Err(_) => {
            let _ = request.respond(text_response(503, "Browser is shutting down"));
            return;
        },
    };
    let Some(reply) = reply else {
        let _ = request.respond(text_response(202, ""));
        return;
    };
    let mut response = Response::from_string(reply)
        .with_header(make_header("Content-Type", "application/json"))
        .with_header(make_header("Cache-Control", "no-store"));
    if is_initialize {
        response.add_header(make_header(SESSION_HEADER, &new_session_id()));
    }
    let _ = request.respond(response);
}

fn new_session_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_checks() {
        assert!(is_allowed_host("127.0.0.1:9223"));
        assert!(is_allowed_host("localhost:9223"));
        assert!(is_allowed_host("LOCALHOST"));
        assert!(is_allowed_host("[::1]:9223"));
        assert!(!is_allowed_host("evil.example:9223"));
        assert!(!is_allowed_host("localhost.evil.example"));
        assert!(is_allowed_origin("http://localhost:3000"));
        assert!(is_allowed_origin("http://127.0.0.1"));
        assert!(!is_allowed_origin("https://evil.example"));
        assert!(!is_allowed_origin("null"));
    }
}
