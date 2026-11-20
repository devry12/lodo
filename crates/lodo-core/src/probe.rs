//! Mengetes koneksi ke kandidat IP dan memilih yang pertama bisa dijangkau.
//!
//! Koneksi TCP yang tersambung saja tidak cukup: beberapa jaringan (mis. hotspot
//! HP) menerima koneksi ke alamat apa pun tanpa ada server di baliknya. Jadi
//! server dianggap terjangkau hanya kalau ia benar-benar menjawab.

use crate::model::{Route, Server};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// IP rumah dites cepat; VPN diberi waktu lebih (peer VPN sering "tidur"
/// dan baru tersambung saat ada lalu lintas).
pub const HOME_TIMEOUT: Duration = Duration::from_millis(600);
pub const VPN_TIMEOUT: Duration = Duration::from_millis(2500);

/// Tunggu sapaan server (SSH, SMTP, FTP, ... menyapa duluan).
const GREETING_WAIT: Duration = Duration::from_millis(700);
/// Tunggu balasan setelah kita "mengetuk" (HTTP dan protokol lain yang menunggu klien).
const REPLY_WAIT: Duration = Duration::from_millis(1500);
/// Ketukan netral: dijawab server web, dan ditolak (dibalas atau ditutup) oleh server lain.
const KNOCK: &[u8] = b"HEAD / HTTP/1.0\r\nHost: lodo\r\n\r\n";
/// Alamat yang pasti tidak ada (TEST-NET-1, RFC 5737), untuk mendeteksi jaringan yang memalsukan koneksi.
const DECOY: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

/// Seberapa yakin ada server sungguhan di balik sebuah alamat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Answer {
    /// Tidak tersambung, atau tersambung tapi diam saja.
    None,
    /// Tersambung lalu ditutup/ditolak oleh pihak seberang, tanpa data.
    Closed,
    /// Pihak seberang mengirim data: pasti ada server.
    Data,
}

/// Sambungkan lalu pastikan pihak seberang menjawab.
pub fn answer(ip: IpAddr, port: u16, connect_timeout: Duration) -> Answer {
    let Ok(mut s) = TcpStream::connect_timeout(&SocketAddr::new(ip, port), connect_timeout) else {
        return Answer::None;
    };
    let mut buf = [0u8; 64];
    let _ = s.set_read_timeout(Some(GREETING_WAIT));
    // Diam: mungkin protokol yang menunggu klien bicara duluan.
    if let Some(a) = read_answer(&mut s, &mut buf) {
        return a;
    }
    if s.write_all(KNOCK).is_err() {
        return Answer::Closed;
    }
    let _ = s.set_read_timeout(Some(REPLY_WAIT));
    read_answer(&mut s, &mut buf).unwrap_or(Answer::None)
}

/// None = belum ada jawaban (habis waktu).
fn read_answer(s: &mut TcpStream, buf: &mut [u8]) -> Option<Answer> {
    match s.read(buf) {
        Ok(0) => Some(Answer::Closed),
        Ok(_) => Some(Answer::Data),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => None,
        Err(_) => Some(Answer::Closed), // reset/ditolak setelah tersambung
    }
}

/// Apakah ada server yang menjawab di alamat ini.
pub fn reachable(ip: IpAddr, port: u16, timeout: Duration) -> bool {
    answer(ip, port, timeout) > Answer::None
}

/// Hanya cek apakah port tersambung (tanpa memastikan jawaban). Untuk penyaringan cepat.
pub fn connects(ip: IpAddr, port: u16, timeout: Duration) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), timeout).is_ok()
}

/// Tes semua kandidat secara paralel, kembalikan hasil per kandidat (urut prioritas).
pub fn probe_all(server: &Server) -> Vec<(Route, IpAddr, bool)> {
    probe_with_decoy(server, DECOY)
}

fn probe_with_decoy(server: &Server, decoy: IpAddr) -> Vec<(Route, IpAddr, bool)> {
    let cands = server.candidates();
    let port = server.port;
    let (tx, rx) = mpsc::channel();
    for (i, (route, ip)) in cands.iter().cloned().enumerate() {
        let tx = tx.clone();
        thread::spawn(move || {
            let t = if route == Route::Home { HOME_TIMEOUT } else { VPN_TIMEOUT };
            let _ = tx.send((i, answer(ip, port, t)));
        });
    }
    // Pada saat yang sama, coba alamat yang pasti tidak ada.
    let decoy_check = thread::spawn(move || answer(decoy, port, HOME_TIMEOUT));
    drop(tx);
    let mut answers = vec![Answer::None; cands.len()];
    for (i, a) in rx {
        answers[i] = a;
    }
    // Kalau alamat palsu pun "menjawab", jaringan ini memalsukan koneksi:
    // hanya kandidat yang benar-benar mengirim data yang dipercaya.
    let lying = decoy_check.join().unwrap_or(Answer::None) > Answer::None;
    let need = if lying { Answer::Data } else { Answer::Closed };
    cands.into_iter().zip(answers).map(|((r, ip), a)| (r, ip, a >= need)).collect()
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

    /// Server tiruan. `greet` = langsung menyapa (seperti SSH); kalau tidak,
    /// baru menjawab setelah menerima data (seperti HTTP). `silent` = tidak pernah
    /// menjawab (seperti hotspot yang memalsukan koneksi).
    fn listen(mode: &'static str) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                thread::spawn(move || match mode {
                    "greet" => {
                        let _ = c.write_all(b"SSH-2.0-test\r\n");
                        thread::sleep(Duration::from_secs(3));
                    }
                    "reply" => {
                        let mut b = [0u8; 64];
                        if c.read(&mut b).unwrap_or(0) > 0 {
                            let _ = c.write_all(b"HTTP/1.0 200 OK\r\n\r\n");
                        }
                    }
                    _ => thread::sleep(Duration::from_secs(5)),
                });
            }
        });
        port
    }

    fn closed_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    }

    #[test]
    fn answers() {
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(answer(lo, listen("greet"), HOME_TIMEOUT), Answer::Data);
        assert_eq!(answer(lo, listen("reply"), HOME_TIMEOUT), Answer::Data);
        assert_eq!(answer(lo, listen("silent"), HOME_TIMEOUT), Answer::None);
        assert_eq!(answer(lo, closed_port(), HOME_TIMEOUT), Answer::None);
    }

    #[test]
    fn prefers_home_when_reachable() {
        let port = listen("greet");
        // 127.0.0.2 juga loopback di Linux, tapi tidak di macOS; pakai IP sama -> Home menang.
        let (r, ip) = resolve(&server("127.0.0.1", "127.0.0.1", port));
        assert_eq!(r, Route::Home);
        assert_eq!(ip, Some("127.0.0.1".parse().unwrap()));
    }

    #[test]
    fn falls_back_to_vpn() {
        let port = listen("reply");
        // Listener hanya di IPv4, jadi ::1 di port yang sama ditolak.
        let (r, _) = resolve(&server("::1", "127.0.0.1", port));
        assert_eq!(r, Route::Vpn("Netbird".into()));
    }

    #[test]
    fn silent_server_is_not_reachable() {
        // Rumah "tersambung" tapi diam (seperti hotspot palsu): tidak dianggap terjangkau.
        let silent = listen("silent");
        let s = server("127.0.0.1", "::1", silent);
        let (r, _) = resolve(&s);
        assert_eq!(r, Route::Unreachable);
    }

    #[test]
    fn lying_network_needs_real_data() {
        // Decoy yang "menjawab" (ditutup tanpa data) menandai jaringan pembohong.
        let decoy_port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let p = l.local_addr().unwrap().port();
            thread::spawn(move || {
                for c in l.incoming().flatten() {
                    drop(c); // tutup langsung: Answer::Closed
                }
            });
            p
        };
        let s = server("127.0.0.1", "127.0.0.1", decoy_port);
        // Tanpa decoy pembohong, "ditutup" dihitung terjangkau.
        assert!(probe_with_decoy(&s, "::1".parse().unwrap())[0].2);
        // Dengan decoy yang juga "ditutup", tidak dipercaya lagi.
        assert!(!probe_with_decoy(&s, "127.0.0.1".parse().unwrap())[0].2);
    }

    #[test]
    fn unreachable_when_none() {
        let (r, ip) = resolve(&server("::1", "127.0.0.1", closed_port()));
        assert_eq!(r, Route::Unreachable);
        assert!(ip.is_none());
    }
}
