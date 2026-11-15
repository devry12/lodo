//! Bagian yang dipakai proses tray maupun proses jendela.

use crate::helper;
use crate::tray::Mood;
use lodo_core::{Route, ServerStatus};
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Port lokal untuk memastikan hanya satu tray / satu jendela yang jalan.
pub const TRAY_LOCK: &str = "127.0.0.1:47812";
pub const WINDOW_LOCK: &str = "127.0.0.1:47813";

/// Ketuk instance yang sudah jalan (minta buka / fokus jendela).
pub fn poke(addr: &str) {
    send_line(addr, "open");
}

/// Kirim satu perintah ke instance yang sudah jalan ("open" atau "update").
pub fn send_line(addr: &str, cmd: &str) -> bool {
    let addr: SocketAddr = addr.parse().unwrap();
    match TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
        Ok(mut s) => s.write_all(format!("{cmd}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Buka alamat web di browser bawaan.
pub fn open_url(url: &str) {
    let _ = if cfg!(target_os = "macos") {
        std::process::Command::new("/usr/bin/open").arg(url).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("explorer.exe").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
}

/// Status terbaru dari helper, diperbarui thread pemantau.
#[derive(Default, Clone, PartialEq)]
pub struct Live {
    pub helper_version: Option<String>,
    pub status: Vec<ServerStatus>,
    /// CA lokal untuk HTTPS sudah dipercaya sistem?
    pub https_trusted: Option<bool>,
}

pub fn mood(live: &Live) -> (Mood, String) {
    if live.helper_version.is_none() {
        return (Mood::Down, "Helper belum aktif".into());
    }
    let s = &live.status;
    if s.is_empty() {
        return (Mood::Idle, Mood::Idle.label().into());
    }
    let down = s.iter().filter(|x| x.route == Route::Unreachable).count();
    if down > 0 {
        return (Mood::Down, format!("{down} server tak terjangkau"));
    }
    if s.iter().all(|x| x.route == Route::Unknown) {
        return (Mood::Idle, "Sedang mengecek…".into());
    }
    if let Some(Route::Vpn(v)) = s.iter().map(|x| &x.route).find(|r| matches!(r, Route::Vpn(_))) {
        return (Mood::Vpn, format!("Lewat {v}"));
    }
    (Mood::Home, Mood::Home.label().into())
}

/// Tanya helper tiap 3 detik. `on_change` dipanggil saat ada perubahan,
/// dengan daftar pesan perpindahan jalur (bisa kosong).
pub fn spawn_poller(live: Arc<Mutex<Live>>, on_change: impl Fn(&[String]) + Send + 'static) {
    thread::spawn(move || {
        loop {
            let helper_version = helper::ping();
            let up = helper_version.is_some();
            let status = if up { helper::status().unwrap_or_default() } else { Vec::new() };
            let https_trusted = if up { helper::https_trusted() } else { None };
            let new = Live { helper_version, status, https_trusted };
            let changed = {
                let mut l = live.lock().unwrap();
                if *l != new {
                    let msgs = route_changes(&l.status, &new.status);
                    *l = new;
                    Some(msgs)
                } else {
                    None
                }
            };
            if let Some(msgs) = changed {
                on_change(&msgs);
            }
            thread::sleep(Duration::from_secs(3));
        }
    });
}

pub fn route_changes(old: &[ServerStatus], new: &[ServerStatus]) -> Vec<String> {
    new.iter()
        .filter_map(|n| {
            let o = old.iter().find(|o| o.fqdn == n.fqdn)?;
            if o.route == n.route || o.route == Route::Unknown || n.route == Route::Unknown {
                return None;
            }
            Some(match &n.route {
                Route::Home => format!("{} sekarang lewat jaringan rumah", n.fqdn),
                Route::Vpn(v) => format!("{} sekarang lewat {v}", n.fqdn),
                Route::Unreachable => format!("{} tidak terjangkau", n.fqdn),
                Route::Unknown => return None,
            })
        })
        .collect()
}

/// Buka otomatis saat login (hanya tray, tanpa jendela).
pub fn autolaunch() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("lodo")
        .set_app_path(&exe.display().to_string())
        .set_args(&["--hidden"])
        .build()
        .ok()
}
