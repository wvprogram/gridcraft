//! Where MCP tool calls end up: a control-channel method call.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};

use gridcraft_engine::control_key;
use std::time::Duration;

use serde_json::{Value, json};

/// Something that answers control-channel methods (`engine.execute`, `engine.commands`,
/// `document.inspect`, `sheet.read`, `ui.screenshot`, …).
pub trait Backend {
    /// Call one method. `Ok` carries the `result`, `Err` the error message.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String>;
    /// True when a real UI is attached (`ui.screenshot` and dialogs work).
    fn has_ui(&self) -> bool;
    /// Short human description ("headless", "connected to 127.0.0.1:7979").
    fn describe(&self) -> String;
}

/// Read/write timeout on the control connection.
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest reply line accepted from the app (guards against a runaway peer).
const MAX_REPLY: u64 = 256 * 1024 * 1024;

/// A running GridCraft app, reached through its loopback control port.
pub struct Remote {
    addr: String,
    conn: Option<(BufReader<TcpStream>, TcpStream)>,
    next_id: u64,
    /// The app's control key, found again on every new connection (a restarted app has a new
    /// one). Only ever sent to loopback addresses.
    key: Option<String>,
}

/// The control key for an app at `sa`: `GRIDCRAFT_CONTROL_KEY`, else the key file the app wrote
/// for its port. `None` for any address that isn't this machine's loopback: the key must not
/// travel to another host.
fn find_key(sa: &SocketAddr, settings: Option<&std::path::Path>) -> Option<String> {
    if !sa.ip().is_loopback() {
        return None;
    }
    let trimmed = |k: String| Some(k.trim().to_string()).filter(|k| !k.is_empty());
    if let Some(k) = std::env::var(control_key::KEY_ENV).ok().and_then(trimmed) {
        return Some(k);
    }
    std::fs::read_to_string(control_key::key_path(settings?, sa.port())).ok().and_then(trimmed)
}

impl Remote {
    /// Connect to `addr` (`127.0.0.1:7979`), failing fast when nothing is listening.
    pub fn connect(addr: &str) -> std::io::Result<Self> {
        let mut r = Self { addr: addr.to_string(), conn: None, next_id: 1, key: None };
        r.reconnect()?;
        Ok(r)
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    fn reconnect(&mut self) -> std::io::Result<()> {
        self.conn = None;
        let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, format!("cannot resolve {}", self.addr));
        for sa in self.addr.to_socket_addrs()? {
            match TcpStream::connect_timeout(&sa, Duration::from_secs(2)) {
                Ok(s) => {
                    s.set_nodelay(true).ok();
                    s.set_read_timeout(Some(IO_TIMEOUT)).ok();
                    s.set_write_timeout(Some(IO_TIMEOUT)).ok();
                    let read = s.try_clone()?;
                    self.key = find_key(&sa, control_key::settings_dir().as_deref());
                    self.conn = Some((BufReader::new(read), s));
                    return Ok(());
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn roundtrip(&mut self, line: &str) -> std::io::Result<String> {
        if self.conn.is_none() {
            self.reconnect()?;
        }
        let Some((reader, writer)) = self.conn.as_mut() else {
            return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "not connected"));
        };
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        let mut reply = String::new();
        let n = std::io::Read::take(reader, MAX_REPLY).read_line(&mut reply)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "control channel closed"));
        }
        Ok(reply)
    }
}

impl Backend for Remote {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let request = |key: &Option<String>| match key {
            Some(k) => json!({"id": id, "key": k, "method": method, "params": params}).to_string(),
            None => json!({"id": id, "method": method, "params": params}).to_string(),
        };
        if self.conn.is_none() {
            // Connect first so the key matches the app now listening.
            let _ = self.reconnect();
        }
        let line = request(&self.key);
        // One retry with a fresh connection (the app may have restarted).
        let reply = match self.roundtrip(&line) {
            Ok(r) => r,
            Err(_) => {
                self.conn = None;
                let _ = self.reconnect();
                let line = request(&self.key);
                self.roundtrip(&line).map_err(|e| {
                    self.conn = None;
                    format!("GridCraft app at {} is not reachable: {e}", self.addr)
                })?
            }
        };
        let v: Value = serde_json::from_str(reply.trim()).map_err(|e| format!("bad reply from app: {e}"))?;
        if v.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let e = v.get("error").and_then(Value::as_str).unwrap_or("unknown error").to_string();
            if e.starts_with("unauthorized") {
                // The app closes a connection after a refused request.
                self.conn = None;
                if self.key.is_none() {
                    return Err(format!(
                        "{e}. No key found for {}: start the app with `gridcraft --control PORT` on this machine (it writes the key file), or set {}",
                        self.addr,
                        control_key::KEY_ENV
                    ));
                }
            }
            Err(e)
        }
    }

    fn has_ui(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("connected to the GridCraft app at {}", self.addr)
    }
}

#[cfg(test)]
mod key_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_key_is_only_for_this_machine() {
        let dir = std::env::temp_dir().join(format!("gridcraft-mcp-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(control_key::key_path(&dir, 7979), "  file-key\n").unwrap();
        let local: SocketAddr = "127.0.0.1:7979".parse().unwrap();
        let local6: SocketAddr = "[::1]:7979".parse().unwrap();
        let remote: SocketAddr = "192.0.2.10:7979".parse().unwrap();
        let other_port: SocketAddr = "127.0.0.1:7980".parse().unwrap();
        if std::env::var_os(control_key::KEY_ENV).is_none() {
            assert_eq!(find_key(&local, Some(&dir)).as_deref(), Some("file-key"));
            assert_eq!(find_key(&local6, Some(&dir)).as_deref(), Some("file-key"));
            assert_eq!(find_key(&other_port, Some(&dir)), None, "each port has its own key file");
            assert_eq!(find_key(&local, None), None);
        }
        assert_eq!(find_key(&remote, Some(&dir)), None, "never sent to another host");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
