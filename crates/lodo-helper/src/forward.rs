//! Penerus port: supaya `http://git.home.arpa` (port 80) sampai ke port
//! aplikasi di server (mis. 3002) tanpa perlu menulis ":3002".
//!
//! File hosts tidak bisa memetakan port. Jadi server yang punya `web_port`
//! diberi alamat loopback sendiri (127.77.0.x). Nama lodo mengarah ke alamat
//! itu, dan helper meneruskan koneksi TCP ke IP server yang sedang dipilih
//! (rumah atau VPN). Karena diteruskan di level TCP, protokol apa pun jalan.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

type Routes = Arc<RwLock<HashMap<SocketAddr, SocketAddr>>>;

#[derive(Default)]
pub struct Forwarder {
    listeners: HashMap<SocketAddr, Arc<AtomicBool>>,
    routes: Routes,
}

impl Forwarder {
    /// Terapkan daftar penerusan: alamat dengar -> tujuan (None = server
    /// sedang tidak terjangkau; koneksi ditolak).
    pub fn apply(&mut self, desired: &HashMap<SocketAddr, Option<SocketAddr>>) {
        *self.routes.write().unwrap() = desired.iter().filter_map(|(l, t)| Some((*l, (*t)?))).collect();

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
                    let routes = self.routes.clone();
                    let addr = *addr;
                    thread::spawn(move || serve(l, addr, routes, stop));
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

fn serve(listener: TcpListener, addr: SocketAddr, routes: Routes, stop: Arc<AtomicBool>) {
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = conn else { continue };
        let target = routes.read().unwrap().get(&addr).copied();
        if let Some(target) = target {
            thread::spawn(move || pipe(client, target));
        }
        // Tanpa tujuan: `client` ditutup, aplikasi langsung tahu gagal.
    }
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

        f.apply(&HashMap::from([(free, Some(home))]));
        assert_eq!(ask(), "home:hello");
        f.apply(&HashMap::from([(free, Some(vpn))]));
        assert_eq!(ask(), "vpn::hello");
        f.apply(&HashMap::from([(free, None)]));
        assert_eq!(ask(), "");
        f.apply(&HashMap::new());
        thread::sleep(Duration::from_millis(100));
        assert!(TcpStream::connect(free).is_err());
    }
}
