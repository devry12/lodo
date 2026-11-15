//! Penerus port: supaya `http://git.home.arpa` (port 80) sampai ke port
//! aplikasi di server (mis. 3002) tanpa perlu menulis ":3002".
//!
//! File hosts tidak bisa memetakan port. Jadi server yang punya `web_port`
//! diberi alamat loopback sendiri (127.77.0.x). Nama lodo mengarah ke alamat
//! itu, dan helper meneruskan koneksi TCP ke IP server yang sedang dipilih
//! (rumah atau VPN). Karena diteruskan di level TCP, protokol apa pun jalan.
//!
//! Port 443 bisa diberi mode TLS: helper membuka HTTPS dengan sertifikat dari
//! CA lokal (lihat ca.rs), lalu meneruskan isinya ke port web di server.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Tujuan sebuah alamat dengar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// None = server sedang tidak terjangkau; koneksi ditolak.
    pub to: Option<SocketAddr>,
    /// Buka TLS di sisi klien (HTTPS lokal) sebelum meneruskan.
    pub tls: bool,
}

/// Alamat loopback untuk server penerusan ke-`n` (mulai 0).
/// `LODO_LOOPBACK` mengganti alamat pertama, untuk pengujian tanpa admin.
pub fn loopback(n: usize) -> Option<Ipv4Addr> {
    let first: Ipv4Addr = std::env::var("LODO_LOOPBACK")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(Ipv4Addr::new(127, 77, 0, 2));
    let [a, b, c, d] = first.octets();
    let last = d as usize + n;
    (last < 255).then(|| Ipv4Addr::new(a, b, c, last as u8))
}

type TlsConfig = Option<Arc<rustls::ServerConfig>>;

#[derive(Default)]
struct Shared {
    routes: HashMap<SocketAddr, (SocketAddr, bool)>,
    tls: TlsConfig,
}

#[derive(Default)]
pub struct Forwarder {
    listeners: HashMap<SocketAddr, Arc<AtomicBool>>,
    shared: Arc<RwLock<Shared>>,
}

impl Forwarder {
    /// Terapkan daftar penerusan. `tls` dipakai untuk alamat bermode TLS;
    /// tanpa `tls`, alamat itu tidak dibuka.
    pub fn apply(&mut self, desired: &HashMap<SocketAddr, Target>, tls: TlsConfig) {
        let desired: HashMap<SocketAddr, Target> =
            desired.iter().filter(|(_, t)| !t.tls || tls.is_some()).map(|(a, t)| (*a, *t)).collect();
        {
            let mut sh = self.shared.write().unwrap();
            sh.routes = desired.iter().filter_map(|(l, t)| Some((*l, (t.to?, t.tls)))).collect();
            sh.tls = tls;
        }

        // Tutup yang tidak dipakai lagi.
        self.listeners.retain(|addr, stop| {
            let keep = desired.contains_key(addr);
            if !keep {
                stop.store(true, Ordering::SeqCst);
                let _ = TcpStream::connect_timeout(addr, Duration::from_millis(200)); // bangunkan accept()
            }
            keep
        });

        // Buka yang baru. Yang gagal dicoba lagi di pengecekan berikutnya.
        for addr in desired.keys() {
            if self.listeners.contains_key(addr) {
                continue;
            }
            match bind(*addr) {
                Ok(l) => {
                    let stop = Arc::new(AtomicBool::new(false));
                    self.listeners.insert(*addr, stop.clone());
                    let shared = self.shared.clone();
                    let addr = *addr;
                    thread::spawn(move || serve(l, addr, shared, stop));
                }
                Err(e) => eprintln!("lodo-helper: tidak bisa membuka {addr}: {e}"),
            }
        }
    }
}

fn bind(addr: SocketAddr) -> io::Result<TcpListener> {
    match TcpListener::bind(addr) {
        Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => {
            // macOS hanya mengaktifkan 127.0.0.1; alamat loopback lain perlu alias.
            add_loopback_alias(addr.ip());
            TcpListener::bind(addr)
        }
        r => r,
    }
}

fn add_loopback_alias(ip: IpAddr) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("/sbin/ifconfig").args(["lo0", "alias", &ip.to_string(), "up"]).output();
    }
}

/// Hapus alias loopback lodo (dipakai saat uninstall).
pub fn remove_loopback_aliases() {
    if cfg!(target_os = "macos") {
        for ip in (0..64).map_while(loopback) {
            let _ = std::process::Command::new("/sbin/ifconfig").args(["lo0", "-alias", &ip.to_string()]).output();
        }
    }
}

fn serve(listener: TcpListener, addr: SocketAddr, shared: Arc<RwLock<Shared>>, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = conn else { continue };
        let (route, tls) = {
            let sh = shared.read().unwrap();
            (sh.routes.get(&addr).copied(), sh.tls.clone())
        };
        match route {
            Some((target, false)) => {
                thread::spawn(move || pipe(client, target));
            }
            Some((target, true)) => {
                if let Some(cfg) = tls {
                    thread::spawn(move || {
                        if let Err(e) = pipe_tls(client, target, cfg) {
                            eprintln!("lodo-helper: koneksi HTTPS: {e}");
                        }
                    });
                }
            }
            // Tanpa tujuan: `client` ditutup, aplikasi langsung tahu gagal.
            None => {}
        }
    }
}

/// HTTPS lokal: selesaikan handshake TLS dengan klien, lalu teruskan isinya
/// (HTTP biasa) ke server. Dua thread berbagi satu sesi TLS lewat mutex;
/// pembacaan socket yang memblok terjadi di luar kunci.
fn pipe_tls(client: TcpStream, target: SocketAddr, cfg: Arc<rustls::ServerConfig>) -> io::Result<()> {
    let mut conn = rustls::ServerConnection::new(cfg).map_err(io::Error::other)?;
    let mut sock = client;
    let _ = sock.set_nodelay(true);
    sock.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock)?;
    }
    sock.set_read_timeout(None)?;

    let server = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT)?;
    let _ = server.set_nodelay(true);
    let conn = Arc::new(Mutex::new(conn));

    // Server -> klien: tulis teks biasa ke sesi TLS, kirim rekaman terenkripsi.
    let down = {
        let conn = conn.clone();
        let mut from = server.try_clone()?;
        let mut to = sock.try_clone()?;
        thread::spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            loop {
                let n = from.read(&mut buf).unwrap_or(0);
                let mut c = conn.lock().unwrap();
                if n == 0 {
                    c.send_close_notify();
                } else if c.writer().write_all(&buf[..n]).is_err() {
                    break;
                }
                while c.wants_write() {
                    if c.write_tls(&mut to).is_err() {
                        return;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _ = to.shutdown(Shutdown::Write);
        })
    };

    // Klien -> server: buka rekaman TLS, teruskan teks biasanya.
    let mut from = sock.try_clone()?;
    let mut to_client = sock;
    let mut to_server = server;
    let mut buf = vec![0u8; 16 * 1024];
    'outer: loop {
        let n = from.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        let mut plain = Vec::new();
        let mut closed = false;
        {
            let mut c = conn.lock().unwrap();
            let mut rd = &buf[..n];
            while !rd.is_empty() {
                if c.read_tls(&mut rd).is_err() {
                    break 'outer;
                }
                if c.process_new_packets().is_err() {
                    while c.wants_write() {
                        let _ = c.write_tls(&mut to_client); // kirim alert ke klien
                    }
                    break 'outer;
                }
                match c.reader().read_to_end(&mut plain) {
                    Ok(_) => closed = true, // close_notify dari klien
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break 'outer,
                }
            }
            while c.wants_write() {
                if c.write_tls(&mut to_client).is_err() {
                    break 'outer;
                }
            }
        }
        if !plain.is_empty() && to_server.write_all(&plain).is_err() {
            break;
        }
        if closed {
            break;
        }
    }
    let _ = to_server.shutdown(Shutdown::Write);
    let _ = down.join();
    Ok(())
}

fn pipe(client: TcpStream, target: SocketAddr) {
    let Ok(server) = TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) else { return };
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let (Ok(c2), Ok(s2)) = (client.try_clone(), server.try_clone()) else { return };
    let up = thread::spawn(move || copy(c2, s2));
    copy(server, client);
    let _ = up.join();
}

/// Salin satu arah sampai selesai, lalu beri tahu sisi lain (half-close).
fn copy(mut from: TcpStream, mut to: TcpStream) {
    let _ = io::copy(&mut from, &mut to);
    let _ = to.shutdown(Shutdown::Write);
    let _ = from.shutdown(Shutdown::Read);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn loopback_addresses() {
        assert_eq!(loopback(0), Some(Ipv4Addr::new(127, 77, 0, 2)));
        assert_eq!(loopback(3), Some(Ipv4Addr::new(127, 77, 0, 5)));
        assert_eq!(loopback(300), None);
    }

    #[test]
    fn forwards_and_switches_target() {
        // Dua "server" echo dengan awalan berbeda.
        let start = |tag: &'static [u8]| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            thread::spawn(move || {
                for mut c in l.incoming().flatten() {
                    let mut buf = [0u8; 5];
                    c.read_exact(&mut buf).unwrap();
                    c.write_all(tag).unwrap();
                    c.write_all(&buf).unwrap();
                }
            });
            a
        };
        let home = start(b"home:");
        let vpn = start(b"vpn::");

        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut f = Forwarder::default();
        let ask = || {
            let mut c = TcpStream::connect(free).unwrap();
            let _ = c.write_all(b"hello");
            let mut s = String::new();
            c.read_to_string(&mut s).ok();
            s
        };

        let plain = |to| HashMap::from([(free, Target { to, tls: false })]);
        f.apply(&plain(Some(home)), None);
        assert_eq!(ask(), "home:hello");
        f.apply(&plain(Some(vpn)), None);
        assert_eq!(ask(), "vpn::hello");
        f.apply(&plain(None), None);
        assert_eq!(ask(), "");
        f.apply(&HashMap::new(), None);
        thread::sleep(Duration::from_millis(100));
        assert!(TcpStream::connect(free).is_err());
    }
}
