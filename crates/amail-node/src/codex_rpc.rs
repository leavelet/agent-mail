//! Minimal JSON-RPC clients for the Codex app-server: over stdio (a
//! throwaway `codex app-server` process) and over the daemon's control
//! socket, which speaks WebSocket on a Unix socket.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const CALL_TIMEOUT: Duration = Duration::from_secs(20);

fn init_params() -> Value {
    json!({
        "clientInfo": {"name": "amail-node", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true},
    })
}

/// Pick the response to request `id` out of a message, if it is one.
fn response(msg: &Value, id: u64) -> Option<Result<Value>> {
    if msg.get("id").and_then(Value::as_u64) != Some(id) || msg.get("method").is_some() {
        return None;
    }
    if let Some(e) = msg.get("error") {
        return Some(Err(anyhow::anyhow!("app-server error: {e}")));
    }
    Some(Ok(msg.get("result").cloned().unwrap_or(Value::Null)))
}

/// Run `calls` against a fresh `codex app-server` over stdio.
pub async fn stdio_calls(
    codex_bin: &str,
    codex_home: &Path,
    calls: &[(&str, Value)],
) -> Result<Vec<Value>> {
    let mut child = tokio::process::Command::new(codex_bin)
        .arg("app-server")
        .env("CODEX_HOME", codex_home)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {codex_bin} app-server"))?;
    let mut stdin = child.stdin.take().context("stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("stdout")?).lines();
    let fut = async {
        let mut out = Vec::new();
        let all = std::iter::once(("initialize", init_params()))
            .chain(calls.iter().map(|(m, p)| (*m, p.clone())));
        for (i, (method, params)) in all.enumerate() {
            let id = i as u64 + 1;
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            stdin.write_all(format!("{line}\n").as_bytes()).await?;
            if id == 1 {
                stdin
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}\n")
                    .await?;
            }
            loop {
                let Some(l) = lines.next_line().await? else {
                    bail!("app-server exited");
                };
                let Ok(msg) = serde_json::from_str::<Value>(&l) else {
                    continue;
                };
                if let Some(r) = response(&msg, id) {
                    if id > 1 {
                        out.push(r?);
                    } else {
                        r?;
                    }
                    break;
                }
            }
        }
        Ok(out)
    };
    let r = tokio::time::timeout(CALL_TIMEOUT, fut)
        .await
        .context("codex app-server timed out")?;
    let _ = child.kill().await;
    r
}

/// Run `calls` against the app-server listening on a Unix socket
/// (WebSocket framing).
pub async fn socket_calls(socket: &Path, calls: &[(&str, Value)]) -> Result<Vec<Value>> {
    let fut = async {
        let mut ws = Ws::connect(socket).await?;
        let mut out = Vec::new();
        let all = std::iter::once(("initialize", init_params()))
            .chain(calls.iter().map(|(m, p)| (*m, p.clone())));
        for (i, (method, params)) in all.enumerate() {
            let id = i as u64 + 1;
            ws.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
                .await?;
            if id == 1 {
                ws.send(&json!({"jsonrpc": "2.0", "method": "initialized"}))
                    .await?;
            }
            loop {
                let msg = ws.recv().await?;
                if let Some(r) = response(&msg, id) {
                    if id > 1 {
                        out.push(r?);
                    } else {
                        r?;
                    }
                    break;
                }
            }
        }
        ws.close().await;
        Ok(out)
    };
    tokio::time::timeout(CALL_TIMEOUT, fut)
        .await
        .context("codex daemon timed out")?
}

/// A WebSocket client over a Unix stream: text frames only, client-masked.
struct Ws {
    s: tokio::net::UnixStream,
}

impl Ws {
    async fn connect(path: &Path) -> Result<Ws> {
        let mut s = crate::paths::connect_unix(path)
            .await
            .with_context(|| format!("connecting to {}", path.display()))?;
        let key = {
            let raw: [u8; 16] = rand::random();
            base64(&raw)
        };
        let req = format!(
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await?;
        let mut hdr = Vec::new();
        while !hdr.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            if s.read(&mut b).await? == 0 {
                bail!("connection closed during handshake");
            }
            hdr.push(b[0]);
            if hdr.len() > 16 * 1024 {
                bail!("handshake response too large");
            }
        }
        let first =
            String::from_utf8_lossy(hdr.split(|&b| b == b'\n').next().unwrap_or(&[])).to_string();
        if !first.contains(" 101 ") {
            bail!("websocket upgrade refused: {}", first.trim());
        }
        Ok(Ws { s })
    }

    async fn frame(&mut self, op: u8, data: &[u8]) -> Result<()> {
        let mask: [u8; 4] = rand::random();
        let mut f = vec![0x80 | op];
        let n = data.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n < 65536 {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        f.extend_from_slice(&mask);
        f.extend(data.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        self.s.write_all(&f).await?;
        Ok(())
    }

    async fn send(&mut self, v: &Value) -> Result<()> {
        self.frame(1, v.to_string().as_bytes()).await
    }

    async fn recv(&mut self) -> Result<Value> {
        let mut msg = Vec::new();
        loop {
            let mut h = [0u8; 2];
            self.s.read_exact(&mut h).await?;
            let (fin, op) = (h[0] & 0x80 != 0, h[0] & 0x0f);
            let mut len = (h[1] & 0x7f) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                self.s.read_exact(&mut b).await?;
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                self.s.read_exact(&mut b).await?;
                len = u64::from_be_bytes(b);
            }
            if len > 64 << 20 {
                bail!("frame too large");
            }
            let mask = if h[1] & 0x80 != 0 {
                let mut m = [0u8; 4];
                self.s.read_exact(&mut m).await?;
                Some(m)
            } else {
                None
            };
            let mut data = vec![0u8; len as usize];
            self.s.read_exact(&mut data).await?;
            if let Some(m) = mask {
                for (i, b) in data.iter_mut().enumerate() {
                    *b ^= m[i % 4];
                }
            }
            match op {
                8 => bail!("connection closed"),
                9 => self.frame(10, &data).await?,
                0..=2 => {
                    msg.extend_from_slice(&data);
                    if fin {
                        match serde_json::from_slice(&msg) {
                            Ok(v) => return Ok(v),
                            Err(_) => msg.clear(),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    async fn close(mut self) {
        let _ = self.frame(8, &[]).await;
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64() {
        assert_eq!(super::base64(b"hello"), "aGVsbG8=");
        assert_eq!(super::base64(b"hi"), "aGk=");
        assert_eq!(super::base64(b"abc"), "YWJj");
    }
}
