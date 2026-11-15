//! Komunikasi app <-> helper lewat TCP localhost (JSON per baris).
//! Hanya mendengarkan di 127.0.0.1; semua config divalidasi helper sebelum dipakai.

use crate::model::{Config, ServerStatus};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

pub const ADDR: &str = "127.0.0.1:47811";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Ping,
    GetConfig,
    SetConfig(Config),
    Status,
    Recheck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Pong { version: String },
    Config(Config),
    Status(Vec<ServerStatus>),
    Ok,
    Error(String),
}

pub fn send(req: &Request) -> Result<Response, String> {
    let addr: SocketAddr = ADDR.parse().unwrap();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|_| "Helper lodo belum berjalan.".to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(15))).ok();
    let mut line = serde_json::to_string(req).map_err(|e| e.to_string())?;
    line.push('\n');
    s.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    BufReader::new(s).read_line(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_str(&buf).map_err(|e| format!("Balasan helper tidak dikenali: {e}"))
}

/// Baca satu request dari koneksi (dipakai helper).
pub fn read_request(s: &TcpStream) -> Option<Request> {
    s.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut buf = String::new();
    BufReader::new(s).take_line(&mut buf)?;
    serde_json::from_str(&buf).ok()
}

pub fn write_response(mut s: &TcpStream, resp: &Response) {
    if let Ok(mut line) = serde_json::to_string(resp) {
        line.push('\n');
        let _ = s.write_all(line.as_bytes());
    }
}

trait TakeLine {
    fn take_line(self, buf: &mut String) -> Option<()>;
}

impl<R: std::io::Read> TakeLine for BufReader<R> {
    fn take_line(self, buf: &mut String) -> Option<()> {
        // Batasi 1 MB supaya tidak bisa dibanjiri.
        use std::io::Read;
        let mut limited = BufReader::new(self.take(1 << 20));
        limited.read_line(buf).ok().filter(|n| *n > 0).map(|_| ())
    }
}
