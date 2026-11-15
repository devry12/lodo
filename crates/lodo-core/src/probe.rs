//! Mengetes koneksi TCP ke kandidat IP dan memilih yang pertama bisa dijangkau.

use crate::model::{Route, Server};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// IP rumah dites cepat; VPN diberi waktu lebih (peer VPN sering "tidur"
/// dan baru tersambung saat ada lalu lintas).
pub const HOME_TIMEOUT: Duration = Duration::from_millis(600);
pub const VPN_TIMEOUT: Duration = Duration::from_millis(2500);

pub fn reachable(ip: IpAddr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout).is_ok()
}

/// Tes semua kandidat secara paralel, kembalikan hasil per kandidat (urut prioritas).
pub fn probe_all(server: &Server) -> Vec<(Route, IpAddr, bool)> {
    let cands = server.candidates();
    let (tx, rx) = mpsc::channel();
    for (i, (route, ip)) in cands.iter().cloned().enumerate() {
        let tx = tx.clone();
        let port = server.port;
        thread::spawn(move || {
            let t = if route == Route::Home { HOME_TIMEOUT } else { VPN_TIMEOUT };
            let _ = tx.send((i, reachable(ip, port, t)));
        });
    }
    drop(tx);
    let mut ok = vec![false; cands.len()];
    for (i, r) in rx {
        ok[i] = r;
    }
    cands.into_iter().zip(ok).map(|((r, ip), ok)| (r, ip, ok)).collect()
}

/// Pilih jalur terbaik: kandidat pertama (urut prioritas) yang bisa dijangkau.
pub fn resolve(server: &Server) -> (Route, Option<IpAddr>) {
    probe_all(server)
        .into_iter()
        .find(|(_, _, ok)| *ok)
        .map(|(r, ip, _)| (r, Some(ip)))
        .unwrap_or((Route::Unreachable, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::VpnTarget;
    use std::net::TcpListener;

    fn server(home: &str, vpn: &str, port: u16) -> Server {
        Server {
            name: "git".into(),
            home_ip: home.into(),
            vpn: vec![VpnTarget { label: "Netbird".into(), ip: vpn.into() }],
            port,
            aliases: vec![],
            web_port: None,
            extra_ports: vec![],
            https: true,
        }
    }

    #[test]
    fn prefers_home_when_reachable() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // 127.0.0.2 juga loopback di Linux, tapi tidak di macOS; pakai IP sama -> Home menang.
        let (r, ip) = resolve(&server("127.0.0.1", "127.0.0.1", port));
        assert_eq!(r, Route::Home);
        assert_eq!(ip, Some("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn falls_back_to_vpn() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        // Listener hanya di IPv4, jadi ::1 di port yang sama ditolak. Sengaja tidak
        // memakai IP luar: beberapa jaringan (mis. hotspot) menerima koneksi ke IP apa pun.
        let (r, _) = resolve(&server("::1", "127.0.0.1", port));
        assert_eq!(r, Route::Vpn("Netbird".into()));
    }

    #[test]
    fn unreachable_when_none() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l); // port tertutup -> connection refused
        let (r, ip) = resolve(&server("::1", "127.0.0.1", port));
        assert_eq!(r, Route::Unreachable);
        assert!(ip.is_none());
    }
}
