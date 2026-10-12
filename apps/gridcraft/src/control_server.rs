//! Localhost JSON-lines control server. Each connection may send many requests; each line
//! `{"id":…, "key":…, "method":…, "params":…}` gets one reply line `{"id":…, "ok":…, …}`.
//!
//! Every request needs the window's key (`docs/control-protocol.md`, Keys): once the port is
//! bound, the app writes a random key to `<settings>/control-key.local-<port>` (mode 0600 on
//! Unix) and removes the file when the window closes. Without the key, anything that can reach
//! `127.0.0.1` (another program, another user, a web page posting to the port) could run any
//! command, including opening and saving files.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use gridcraft_engine::control_key;
use gridcraft_ui_egui::ControlRequest;
use serde_json::{Value as Json, json};

const MAX_LINE: usize = 64 * 1024 * 1024;

/// The key file of a running control server; [`KeyFile::remove`] it when the window closes.
pub struct KeyFile(PathBuf);

impl KeyFile {
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bind `127.0.0.1:port`, write the key file into `settings` and serve. Without a settings
/// folder or a random key, the key file can't be shared and every request is refused.
pub fn start(port: u16, ctx: egui::Context, settings: Option<&Path>) -> std::io::Result<(Receiver<ControlRequest>, Option<KeyFile>)> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let mut key = String::new();
    let mut key_file = None;
    match (random_key(), settings) {
        (Ok(k), Some(dir)) => {
            let p = control_key::key_path(dir, port);
            match write_key_file(&p, &k) {
                Ok(()) => {
                    log::info!("control channel listening on 127.0.0.1:{port}, key in {}", p.display());
                    key = k;
                    key_file = Some(KeyFile(p));
                }
                Err(e) => log::error!("control key file not written ({e}): requests will be refused"),
            }
        }
        (Err(e), _) => log::error!("no random key for the control channel ({e}): requests will be refused"),
        (_, None) => log::error!("no settings folder for the control key file: requests will be refused"),
    }
    let (tx, rx) = channel();
    let key: Arc<str> = key.into();
    std::thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (tx, ctx, key) = (tx.clone(), ctx.clone(), key.clone());
            let _ = std::thread::Builder::new().name("control-conn".into()).spawn(move || serve(stream, tx, ctx, &key));
        }
    })?;
    Ok((rx, key_file))
}

/// A new key: 32 random bytes from the operating system, as hex.
fn random_key() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| e.to_string())?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Write the key file: a stale one (from a run that didn't exit cleanly) is replaced; on Unix the
/// new file is created with mode 0600 (only the user can read it).
fn write_key_file(p: &Path, key: &str) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let _ = std::fs::remove_file(p);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let mut f = open.open(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    f.write_all(key.as_bytes()).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// The next line, at most [`MAX_LINE`] bytes: `None` at the end of the stream or after a read
/// error, `Some(Err)` for a longer or non-UTF-8 line (read only as far as the limit).
fn read_line(r: &mut BufReader<TcpStream>) -> Option<Result<String, &'static str>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).ok()?;
    if n == 0 {
        return None;
    }
    if buf.len() > MAX_LINE {
        return Some(Err("request too large"));
    }
    Some(String::from_utf8(buf).map_err(|_| "request is not UTF-8"))
}

/// The reply to a request that has no key or the wrong one.
const UNAUTHORIZED: &str = "unauthorized: every request needs the window's control key (`key`); see docs/control-protocol.md, Keys";

fn serve(stream: TcpStream, tx: Sender<ControlRequest>, ctx: egui::Context, key: &str) {
    let Ok(read) = stream.try_clone() else { return };
    let mut out = stream;
    let mut reader = BufReader::new(read);
    while let Some(line) = read_line(&mut reader) {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                // The rest of an oversize line can't be told apart from the next request.
                let _ = writeln!(out, "{}", json!({"ok": false, "error": e}));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Json = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"ok": false, "error": format!("invalid JSON: {e}")}));
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        if !msg.get("key").and_then(Json::as_str).is_some_and(|k| control_key::keys_match(k, key)) {
            // Close the connection: a client without the key has no business here.
            let _ = writeln!(out, "{}", json!({"id": id, "ok": false, "error": UNAUTHORIZED}));
            break;
        }
        let method = msg.get("method").and_then(Json::as_str).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let (req, rx) = ControlRequest::new(method, params);
        if tx.send(req).is_err() {
            break;
        }
        // Wake the UI thread: egui only runs frames when something happens.
        ctx.request_repaint();
        let mut reply = rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| json!({"ok": false, "error": "timed out waiting for the UI (is the window minimised?)"}));
        if let Some(o) = reply.as_object_mut() {
            o.insert("id".into(), id);
        }
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// A server on a free port (with a test key), answering every forwarded request with
    /// `{"ok": true, "result": method}` the way the UI thread does.
    fn server(key: &str) -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = channel::<ControlRequest>();
        let key: Arc<str> = key.into();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, key) = (tx.clone(), key.clone());
                std::thread::spawn(move || serve(stream, tx, egui::Context::default(), &key));
            }
        });
        std::thread::spawn(move || {
            for req in rx {
                let _ = req.reply.send(json!({"ok": true, "result": req.method}));
            }
        });
        port
    }

    fn exchange(port: u16, lines: &[&str]) -> Vec<Json> {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        for l in lines {
            s.write_all(l.as_bytes()).unwrap();
            s.write_all(b"\n").unwrap();
        }
        s.shutdown(std::net::Shutdown::Write).unwrap();
        BufReader::new(s).lines().map_while(Result::ok).filter_map(|l| serde_json::from_str(&l).ok()).collect()
    }

    #[test]
    fn requests_need_the_key() {
        let port = server("k3y");
        let ok = exchange(port, &[r#"{"id":1,"key":"k3y","method":"document.inspect"}"#, r#"{"id":2,"key":"k3y","method":"sheet.read"}"#]);
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0]["ok"], true);
        assert_eq!(ok[1]["result"], "sheet.read");
        for bad in [
            r#"{"id":1,"method":"engine.execute"}"#,
            r#"{"id":1,"key":"k3x","method":"engine.execute"}"#,
            r#"{"id":1,"key":"","method":"engine.execute"}"#,
        ] {
            let r = exchange(port, &[bad, r#"{"id":2,"key":"k3y","method":"engine.execute"}"#]);
            assert_eq!(r.len(), 1, "{bad}: the connection closes after a refused request: {r:?}");
            assert_eq!(r[0]["ok"], false);
            assert!(r[0]["error"].as_str().unwrap().starts_with("unauthorized"), "{r:?}");
        }
    }

    /// A web page can POST to the port: the request line and headers are invalid JSON, and the
    /// body's command has no key, so nothing runs.
    #[test]
    fn a_browser_post_runs_nothing() {
        let port = server("k3y");
        let post = "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: text/plain\r\n\r\n{\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"file.save\",\"params\":{\"path\":\"/tmp/x.html\"}}}";
        let r = exchange(port, &[post]);
        assert!(r.iter().all(|v| v["ok"] == false), "{r:?}");
        assert!(r.iter().any(|v| v["error"].as_str().unwrap_or("").starts_with("unauthorized")), "{r:?}");
    }

    #[test]
    fn without_a_key_file_every_request_is_refused() {
        let port = server("");
        let r = exchange(port, &[r#"{"id":1,"key":"","method":"document.inspect"}"#]);
        assert_eq!(r[0]["ok"], false);
    }

    #[test]
    fn key_files_are_private_and_replace_stale_ones() {
        let dir = std::env::temp_dir().join(format!("gridcraft-key-{}", std::process::id()));
        let p = control_key::key_path(&dir, 7979);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&p, "stale").unwrap();
        write_key_file(&p, "fresh").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fresh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        KeyFile(p.clone()).remove();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(random_key().unwrap().len(), 64);
        assert_ne!(random_key().unwrap(), random_key().unwrap());
    }
}
