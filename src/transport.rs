use crate::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::{net::TcpStream, time::Duration};
use tungstenite::{
    Message,
    WebSocket,
    client::connect as ws_connect,
    stream::MaybeTlsStream,
};

/// Bound on a single blocking socket read so a wedged browser cannot hang the caller forever.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A JSON-RPC-with-ids channel over one WebSocket, shared by CDP and `BiDi`. One in-flight
/// request at a time, matched by a monotonically increasing id.
pub struct Wire {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Wire {
    pub fn connect(ws_url: &str) -> Result<Self> {
        let (mut socket, _response) = ws_connect(ws_url).map_err(Error::Transport)?;
        // TLS is compiled out, so the stream is always `Plain`; bound its reads regardless.
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream.set_read_timeout(Some(READ_TIMEOUT))?;
        }
        Ok(Self { socket, next_id: 0 })
    }

    /// Send `object` (with an injected id) and return the response frame with the matching id,
    /// discarding events and unrelated frames along the way.
    pub fn request(&mut self, mut object: Map<String, Value>) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        object.insert("id".to_owned(), json!(id));
        self.socket
            .send(Message::text(Value::Object(object).to_string()))
            .map_err(Error::Transport)?;
        loop {
            let message = self.socket.read().map_err(Error::Transport)?;
            if message.is_close() {
                return Err(Error::Malformed {
                    protocol: "ws",
                    detail: "connection closed by the browser".to_owned(),
                });
            }
            if !matches!(message, Message::Text(_)) {
                continue;
            }
            let text = message.to_text().map_err(Error::Transport)?;
            let frame: Value =
                serde_json::from_str(text).map_err(|e| Error::Malformed {
                    protocol: "ws",
                    detail: e.to_string(),
                })?;
            if frame.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(frame);
            }
        }
    }
}

/// Build a `{ method, params, [extra] }` command object; the id is added by [`Wire::request`].
pub fn command(
    method: &str,
    params: Value,
    extra: Option<(&str, &str)>,
) -> Map<String, Value> {
    let mut object = Map::new();
    object.insert("method".to_owned(), json!(method));
    object.insert("params".to_owned(), params);
    if let Some((key, value)) = extra {
        object.insert(key.to_owned(), json!(value));
    }
    object
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Wire, command};
    use serde_json::{Value, json};
    use std::{net::TcpListener, thread};
    use tungstenite::{Message, accept};

    #[test]
    fn request_skips_events_and_matches_id() {
        let listener = TcpListener::bind((crate::LOOPBACK, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = accept(stream).unwrap();
            let request = ws.read().unwrap();
            let frame: Value =
                serde_json::from_str(request.to_text().unwrap()).unwrap();
            let id = frame.get("id").and_then(Value::as_u64).unwrap();
            ws.send(Message::text(json!({ "method": "some.event" }).to_string()))
                .unwrap();
            ws.send(Message::text(
                json!({ "id": id, "result": { "ok": true } }).to_string(),
            ))
            .unwrap();
        });

        let mut wire = Wire::connect(&format!("ws://{addr}/")).unwrap();
        let frame = wire
            .request(command("test.method", json!({}), None))
            .unwrap();
        server.join().unwrap();

        assert_eq!(frame.get("id").and_then(Value::as_u64), Some(1));
        assert_eq!(
            frame.pointer("/result/ok").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn request_ids_increase_monotonically() {
        let listener = TcpListener::bind((crate::LOOPBACK, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut ws = accept(stream).unwrap();
            for _ in 0..2 {
                let request = ws.read().unwrap();
                let frame: Value =
                    serde_json::from_str(request.to_text().unwrap()).unwrap();
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                ws.send(Message::text(json!({ "id": id, "result": {} }).to_string()))
                    .unwrap();
            }
        });

        let mut wire = Wire::connect(&format!("ws://{addr}/")).unwrap();
        let first = wire.request(command("a", json!({}), None)).unwrap();
        let second = wire.request(command("b", json!({}), None)).unwrap();
        server.join().unwrap();

        assert_eq!(first.get("id").and_then(Value::as_u64), Some(1));
        assert_eq!(second.get("id").and_then(Value::as_u64), Some(2));
    }
}
