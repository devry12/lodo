//! lodo-helper: service kecil berhak admin yang memilih IP terbaik
//! dan menulis blok lodo di file hosts.
//!
//!   lodo-helper run        jalan sebagai service (dipanggil launchd/systemd/Task Scheduler)
//!   lodo-helper install    pasang helper sebagai service (butuh admin)
//!   lodo-helper uninstall  hapus service + bersihkan hosts (butuh admin)
//!   lodo-helper update     perbarui binary ini ke versi terbaru (tanpa restart)
#![cfg_attr(windows, windows_subsystem = "windows")]

mod forward;
mod service;
mod selfupdate;

use lodo_core::ipc::{self, Request, Response};
use lodo_core::{hosts, probe, serde_json, Config, Route, ServerStatus};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const CHECK_EVERY: Duration = Duration::from_secs(30);
const NET_POLL: Duration = Duration::from_secs(3);

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let res = match cmd.as_str() {
        "run" => run(),
        "install" => service::install(),
        "uninstall" => service::uninstall(),
        "update" => selfupdate::update_cli(),
        "--version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => Err("Pemakaian: lodo-helper run | install | uninstall".into()),
    };
    if let Err(e) = res {
        eprintln!("lodo-helper: {e}");
        std::process::exit(1);
    }
}

/// Lokasi config helper. `LODO_CONFIG` bisa dipakai untuk pengujian.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LODO_CONFIG") {
        return PathBuf::from(p);
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/lodo/config.json")
    } else if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        PathBuf::from(base).join(r"lodo\config.json")
    } else {
        PathBuf::from("/etc/lodo/config.json")
    }
}

fn load_config() -> Config {
    std::fs::read(config_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Config>(&b).ok())
        .filter(|c| c.validate().is_ok())
        .unwrap_or_default()
}

fn save_config(c: &Config) -> Result<(), String> {
    let p = config_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec_pretty(c).map_err(|e| e.to_string())?;
    std::fs::write(&p, json).map_err(|e| format!("Gagal menyimpan config: {e}"))
}

struct State {
    config: Config,
    status: Vec<ServerStatus>,
}

fn unknown_status(c: &Config) -> Vec<ServerStatus> {
    c.servers
        .iter()
        .map(|s| ServerStatus { name: s.name.clone(), fqdn: s.fqdn(&c.domain), ip: None, route: Route::Unknown })
        .collect()
}

fn run() -> Result<(), String> {
    let config = load_config();
    let state = Arc::new(Mutex::new(State { status: unknown_status(&config), config }));
    let (tx, rx) = mpsc::channel::<()>();

    // Coba beberapa kali: setelah pembaruan, proses lama mungkin belum melepas port.
    let mut listener = TcpListener::bind(ipc::addr());
    for _ in 0..40 {
        if listener.is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(500));
        listener = TcpListener::bind(ipc::addr());
    }
    let listener = listener.map_err(|e| format!("Tidak bisa membuka {}: {e}", ipc::addr()))?;

    // Pembaruan otomatis helper (hanya untuk helper yang terpasang sebagai service).
    let update_tx = selfupdate::spawn();

    // Pantau perubahan jaringan (ganti WiFi, VPN nyala/mati).
    {
        let tx = tx.clone();
        thread::spawn(move || watch_network(tx));
    }
    // Server IPC untuk app.
    {
        let state = state.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let state = state.clone();
                let tx = tx.clone();
                let update_tx = update_tx.clone();
                thread::spawn(move || handle(conn, &state, &tx, &update_tx));
            }
        });
    }

    // Loop utama: cek saat dipicu atau tiap 30 detik.
    let mut fwd = forward::Forwarder::default();
    let mut first = true;
    loop {
        if !first {
            match rx.recv_timeout(CHECK_EVERY) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
            while rx.try_recv().is_ok() {} // gabungkan pemicu beruntun
        }
        first = false;
        check_once(&state, &mut fwd);
    }
}

fn check_once(state: &Arc<Mutex<State>>, fwd: &mut forward::Forwarder) {
    let config = state.lock().unwrap().config.clone();
    // Cek semua server paralel.
    let handles: Vec<_> = config
        .servers
        .iter()
        .cloned()
        .map(|s| thread::spawn(move || (s.clone(), probe::resolve(&s))))
        .collect();
    let mut status = Vec::new();
    let mut entries = Vec::new();
    let mut forwards = HashMap::new();
    let mut n_loopback = 0;
    for h in handles {
        let Ok((s, (route, ip))) = h.join() else { continue };
        let ports = s.forwards();
        let lo = if ports.is_empty() { None } else { forward::loopback(n_loopback) };
        if let Some(lo) = lo {
            // Nama selalu mengarah ke alamat loopback; helper yang meneruskan.
            n_loopback += 1;
            entries.push((lo.to_string(), s.all_names(&config.domain)));
            for (listen, target) in ports {
                forwards.insert(SocketAddr::new(IpAddr::V4(lo), listen), ip.map(|ip| SocketAddr::new(ip, target)));
            }
        } else if let Some(ip) = ip {
            entries.push((ip.to_string(), s.all_names(&config.domain)));
        }
        status.push(ServerStatus { name: s.name.clone(), fqdn: s.fqdn(&config.domain), ip: ip.map(|i| i.to_string()), route });
    }
    fwd.apply(&forwards);
    match hosts::update_file(&hosts::hosts_path(), &entries) {
        Ok(true) => hosts::flush_dns(),
        Ok(false) => {}
        Err(e) => eprintln!("lodo-helper: gagal menulis hosts: {e}"),
    }
    let mut st = state.lock().unwrap();
    // Abaikan hasil kalau config berubah selama pengecekan; pengecekan baru sudah dipicu.
    if st.config == config {
        st.status = status;
    }
}

fn handle(conn: std::net::TcpStream, state: &Arc<Mutex<State>>, tx: &Sender<()>, update_tx: &Sender<()>) {
    let Some(req) = ipc::read_request(&conn) else { return };
    let resp = match req {
        Request::Ping => Response::Pong { version: env!("CARGO_PKG_VERSION").into() },
        Request::GetConfig => Response::Config(state.lock().unwrap().config.clone()),
        Request::Status => Response::Status(state.lock().unwrap().status.clone()),
        Request::Recheck => {
            let _ = tx.send(());
            Response::Ok
        }
        Request::Update => {
            let _ = update_tx.send(());
            Response::Ok
        }
        Request::SetConfig(mut c) => {
            c.domain = c.domain.trim().trim_end_matches('.').to_ascii_lowercase();
            match c.validate().and_then(|_| save_config(&c)) {
                Ok(()) => {
                    let mut st = state.lock().unwrap();
                    // Pertahankan status server yang tidak berubah.
                    let old = std::mem::take(&mut st.status);
                    st.status = unknown_status(&c)
                        .into_iter()
                        .map(|u| old.iter().find(|o| o.fqdn == u.fqdn).cloned().unwrap_or(u))
                        .collect();
                    st.config = c;
                    let _ = tx.send(());
                    Response::Ok
                }
                Err(e) => Response::Error(e),
            }
        }
    };
    ipc::write_response(&conn, &resp);
}

fn net_snapshot() -> Vec<String> {
    let mut v: Vec<String> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| format!("{}={}", i.name, i.ip()))
        .collect();
    v.sort();
    v
}

fn watch_network(tx: Sender<()>) {
    let mut last = net_snapshot();
    loop {
        thread::sleep(NET_POLL);
        let now = net_snapshot();
        if now != last {
            // Tunggu sebentar sampai jaringan stabil (DHCP, VPN handshake).
            thread::sleep(Duration::from_secs(2));
            last = net_snapshot();
            if tx.send(()).is_err() {
                return;
            }
        }
    }
}
