use crate::{
    Cookie,
    Engine,
    discover,
    error::{Error, Result},
    transport::{Wire, command},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const ENV_VAR: &str = "CHROME";

pub fn find() -> Result<PathBuf> {
    discover::find(Engine::Chrome, ENV_VAR, "chrome")
}

pub fn spawn(bin: &Path, profile_dir: &Path) -> Result<Child> {
    Command::new(bin)
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(Error::Launch)
}

/// Chrome writes `DevToolsActivePort` (line 1: port, line 2: browser ws path) once its debug
/// endpoint is up. Poll it into a full `ws://` URL.
fn ws_url(profile_dir: &Path, timeout: Duration) -> Result<String> {
    let port_file = profile_dir.join("DevToolsActivePort");
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(&port_file) {
            let mut lines = contents.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://{}:{port}{path}", crate::LOOPBACK));
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::EndpointTimeout(timeout));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn call(
    wire: &mut Wire,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Result<Value> {
    let frame =
        wire.request(command(method, params, session.map(|s| ("sessionId", s))))?;
    if let Some(error) = frame.get("error") {
        return Err(Error::Command {
            protocol: "cdp",
            method: method.to_owned(),
            message: error.to_string(),
        });
    }
    Ok(frame.get("result").cloned().unwrap_or(Value::Null))
}

fn as_str(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| Error::Malformed {
            protocol: "cdp",
            detail: format!("missing `{key}`"),
        })
}

/// Connect, open the login page in an attached target, and enable the network domain. Returns the
/// live wire and the attached session id used for subsequent cookie reads.
pub fn connect(
    profile_dir: &Path,
    login_url: &str,
    timeout: Duration,
) -> Result<(Wire, String)> {
    let mut wire = Wire::connect(&ws_url(profile_dir, timeout)?)?;
    let target = call(
        &mut wire,
        "Target.createTarget",
        json!({ "url": login_url }),
        None,
    )?;
    let target_id = as_str(&target, "targetId")?;
    let attached = call(
        &mut wire,
        "Target.attachToTarget",
        json!({ "targetId": target_id, "flatten": true }),
        None,
    )?;
    let session_id = as_str(&attached, "sessionId")?;
    call(&mut wire, "Network.enable", json!({}), Some(&session_id))?;
    Ok((wire, session_id))
}

pub fn cookies(wire: &mut Wire, session: &str) -> Result<Vec<Cookie>> {
    let result = call(wire, "Network.getAllCookies", json!({}), Some(session))?;
    let cookies = result
        .get("cookies")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    serde_json::from_value(cookies).map_err(Error::DecodeCookies)
}

pub fn close(wire: &mut Wire) {
    // Graceful teardown so Chrome tears down its own process tree; the socket may drop first.
    let _ = call(wire, "Browser.close", json!({}), None);
}
