use crate::{
    Cookie,
    discover,
    error::{Error, Result},
    transport::{Wire, command},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const ENV_VAR: &str = "FIREFOX";

pub fn find() -> Result<PathBuf> {
    discover::find(crate::Engine::Firefox, ENV_VAR, "firefox")
}

/// Reserve a free loopback port by binding to 0, then release it for Firefox to claim.
pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind((crate::LOOPBACK, 0))?;
    Ok(listener.local_addr()?.port())
}

pub fn spawn(bin: &Path, profile_dir: &Path, port: u16) -> Result<Child> {
    Command::new(bin)
        .arg("--remote-debugging-port")
        .arg(port.to_string())
        .arg("--profile")
        .arg(profile_dir)
        .arg("--no-remote")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::Launch)
}

fn call(wire: &mut Wire, method: &str, params: Value) -> Result<Value> {
    let frame = wire.request(command(method, params, None))?;
    match frame.get("type").and_then(Value::as_str) {
        Some("success") => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
        _ => Err(Error::Command {
            protocol: "bidi",
            method: method.to_owned(),
            message: frame.to_string(),
        }),
    }
}

/// The `BiDi` websocket path is fixed (`/session`), so we pin the port and retry the connect until
/// Firefox's remote agent starts listening, then open a session and navigate to the login URL.
pub fn connect(port: u16, login_url: &str, timeout: Duration) -> Result<Wire> {
    let ws_url = format!("ws://{}:{port}/session", crate::LOOPBACK);
    let deadline = Instant::now() + timeout;
    let mut wire = loop {
        match Wire::connect(&ws_url) {
            Ok(wire) => break wire,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return Err(Error::EndpointTimeout(timeout)),
        }
    };
    call(&mut wire, "session.new", json!({ "capabilities": {} }))?;
    navigate(&mut wire, login_url)?;
    Ok(wire)
}

/// Open `login_url` in the first browsing context. Unlike Chrome's `Target.createTarget`, `BiDi`
/// does not take a URL at session creation, so we drive navigation explicitly.
fn navigate(wire: &mut Wire, login_url: &str) -> Result<()> {
    let tree = call(wire, "browsingContext.getTree", json!({}))?;
    let context = tree
        .get("contexts")
        .and_then(Value::as_array)
        .and_then(|contexts| contexts.first())
        .and_then(|first| first.get("context"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Malformed {
            protocol: "bidi",
            detail: "no browsing context to navigate".to_owned(),
        })?;
    call(
        wire,
        "browsingContext.navigate",
        json!({ "context": context, "url": login_url, "wait": "none" }),
    )?;
    Ok(())
}

/// Map one `BiDi` cookie (whose value is a `{ type, value }` `BytesValue`) into our shape.
fn cookie(raw: &Value) -> Result<Cookie> {
    let field = |key: &str| raw.get(key).and_then(Value::as_str);
    let missing = |detail: String| Error::Malformed {
        protocol: "bidi",
        detail,
    };
    let name = field("name")
        .ok_or_else(|| missing("cookie missing name".to_owned()))?
        .to_owned();

    let bytes = raw
        .get("value")
        .ok_or_else(|| missing(format!("cookie `{name}` missing value")))?;
    let encoding = bytes
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("string");
    let encoded = bytes
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| missing(format!("cookie `{name}` value not a string")))?;
    let value = match encoding {
        "string" => encoded.to_owned(),
        "base64" => {
            let bytes = STANDARD.decode(encoded).map_err(|e| {
                missing(format!("cookie `{name}` base64 decode failed: {e}"))
            })?;
            String::from_utf8(bytes).map_err(|_| Error::UnsupportedCookieEncoding {
                encoding: "base64 (non-utf8 bytes)".to_owned(),
            })?
        }
        other => {
            return Err(Error::UnsupportedCookieEncoding {
                encoding: other.to_owned(),
            });
        }
    };

    let expiry = raw.get("expiry").and_then(Value::as_f64);
    Ok(Cookie {
        name,
        value,
        domain: field("domain").unwrap_or_default().to_owned(),
        path: field("path").unwrap_or("/").to_owned(),
        expires: expiry.unwrap_or(-1.0),
        http_only: raw
            .get("httpOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        secure: raw.get("secure").and_then(Value::as_bool).unwrap_or(false),
        session: expiry.is_none(),
    })
}

pub fn cookies(wire: &mut Wire) -> Result<Vec<Cookie>> {
    let result = call(wire, "storage.getCookies", json!({}))?;
    result
        .get("cookies")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(cookie)
        .collect()
}

pub fn close(wire: &mut Wire) {
    let _ = call(wire, "browser.close", json!({}));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::cookie;
    use serde_json::json;

    #[test]
    fn maps_string_value() {
        let raw = json!({
            "name": "SID", "value": { "type": "string", "value": "abc" },
            "domain": "corp.example", "path": "/", "secure": true, "httpOnly": true,
            "expiry": 1_800_000_000.0,
        });
        assert_eq!(cookie(&raw).unwrap().value, "abc");
    }

    #[test]
    fn marks_session_when_no_expiry() {
        let raw = json!({
            "name": "SID", "value": { "type": "string", "value": "abc" },
            "domain": "corp.example", "path": "/",
        });
        assert!(cookie(&raw).unwrap().session);
    }

    #[test]
    fn decodes_base64_value() {
        let raw = json!({
            "name": "SID", "value": { "type": "base64", "value": "YWJj" },
            "domain": "corp.example", "path": "/",
        });
        assert_eq!(cookie(&raw).unwrap().value, "abc");
    }

    #[test]
    fn rejects_non_utf8_base64_value() {
        let raw = json!({
            "name": "SID", "value": { "type": "base64", "value": "/w==" },
            "domain": "corp.example", "path": "/",
        });
        assert!(cookie(&raw).is_err());
    }

    #[test]
    fn rejects_unknown_encoding() {
        let raw = json!({
            "name": "SID", "value": { "type": "weird", "value": "x" },
            "domain": "corp.example", "path": "/",
        });
        assert!(cookie(&raw).is_err());
    }
}
