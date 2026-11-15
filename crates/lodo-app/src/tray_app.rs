//! Proses tray: sangat ringan (tanpa jendela / GPU). Jendela pengaturan
//! dijalankan sebagai proses terpisah hanya saat dibuka, lalu selesai saat ditutup.

use crate::shared::{self, Live, TRAY_LOCK, WINDOW_LOCK};
use crate::tray::Tray;
use crate::helper;
use crate::updater;
use lodo_core::update::Manifest;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tray_icon::menu::{MenuEvent, MenuId};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

enum Ev {
    Menu(MenuId),
    Open,
    Refresh,
    /// Versi baru tersedia tapi tidak dipasang otomatis.
    UpdateAvailable(String),
    /// Pasang pembaruan sekarang (true = buka jendela setelah dibuka ulang).
    UpdateNow(bool),
    UpdateFailed(String),
    /// Pembaruan terpasang: keluar supaya versi baru bisa jalan.
    Restart,
}

const UPDATE_FIRST: Duration = Duration::from_secs(15);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

struct TrayApp {
    tray: Option<Tray>,
    live: Arc<Mutex<Live>>,
    window: Option<Child>,
    open_on_start: bool,
    proxy: winit::event_loop::EventLoopProxy<Ev>,
    pending: Arc<Mutex<Option<Manifest>>>,
    updating: bool,
}

pub fn run(open_window: bool) {
    // Hanya satu tray. Kalau sudah ada, minta tray itu membuka jendela.
    // LODO_TRAY_LOCK: jalankan tray kedua untuk pengujian.
    let lock = std::env::var("LODO_TRAY_LOCK").unwrap_or_else(|_| TRAY_LOCK.into());
    let Ok(listener) = TcpListener::bind(&lock) else {
        shared::poke(&lock);
        return;
    };

    // Beri tahu bundle id langsung supaya tidak perlu mencarinya lewat AppleScript.
    #[cfg(target_os = "macos")]
    let _ = notify_rust::set_application("id.lodo.app");

    let mut builder = EventLoop::<Ev>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    {
        let proxy = proxy.clone();
        thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut line = String::new();
                let _ = BufReader::new(conn).read_line(&mut line);
                let ev = if line.trim() == "update" { Ev::UpdateNow(true) } else { Ev::Open };
                let _ = proxy.send_event(ev);
            }
        });
    }
    {
        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = proxy.send_event(Ev::Menu(e.id));
        }));
    }
    // Windows/Linux: klik kiri ikon langsung membuka jendela (macOS menampilkan menu).
    #[cfg(not(target_os = "macos"))]
    {
        let proxy = proxy.clone();
        tray_icon::TrayIconEvent::set_event_handler(Some(move |e: tray_icon::TrayIconEvent| {
            if let tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                button_state: tray_icon::MouseButtonState::Up,
                ..
            } = e
            {
                let _ = proxy.send_event(Ev::Open);
            }
        }));
    }

    let live = Arc::new(Mutex::new(Live::default()));
    {
        let proxy = proxy.clone();
        shared::spawn_poller(live.clone(), move |changes| {
            if !changes.is_empty() {
                notify(&changes.join("\n"));
            }
            let _ = proxy.send_event(Ev::Refresh);
        });
    }

    let pending = Arc::new(Mutex::new(None));
    spawn_update_loop(proxy.clone(), pending.clone());

    let mut app = TrayApp { tray: None, live, window: None, open_on_start: open_window, proxy, pending, updating: false };
    let _ = event_loop.run_app(&mut app);
}

/// Tampilkan notifikasi di thread terpisah: di macOS pustaka notifikasi bisa
/// memanggil AppleScript yang sesekali macet, dan itu tidak boleh menahan tray.
fn notify(body: &str) {
    let body = body.to_string();
    thread::spawn(move || {
        let _ = notify_rust::Notification::new().summary("lodo").body(&body).show();
    });
}

/// Periksa pembaruan saat mulai lalu tiap 6 jam.
fn spawn_update_loop(proxy: winit::event_loop::EventLoopProxy<Ev>, pending: Arc<Mutex<Option<Manifest>>>) {
    thread::spawn(move || {
        thread::sleep(UPDATE_FIRST);
        loop {
            if let Ok(Some(m)) = updater::check() {
                let version = m.version.clone();
                *pending.lock().unwrap() = Some(m);
                let ev = if updater::auto_update() && updater::can_self_install() {
                    Ev::UpdateNow(false)
                } else {
                    Ev::UpdateAvailable(version)
                };
                let _ = proxy.send_event(ev);
            }
            thread::sleep(UPDATE_EVERY);
        }
    });
}

impl TrayApp {
    fn start_update(&mut self, open_window: bool) {
        if self.updating {
            return;
        }
        if !updater::can_self_install() {
            shared::open_url(&updater::download_page());
            return;
        }
        self.updating = true;
        if let Some(t) = &self.tray {
            t.update_item.set_text("Memperbarui…");
            t.update_item.set_enabled(false);
        }
        let proxy = self.proxy.clone();
        let pending = self.pending.lock().unwrap().clone();
        thread::spawn(move || {
            let m = match pending {
                Some(m) => Ok(Some(m)),
                None => updater::check(),
            };
            let ev = match m {
                Ok(Some(m)) => match updater::install(&m, open_window) {
                    Ok(()) => Ev::Restart,
                    Err(e) => Ev::UpdateFailed(e),
                },
                Ok(None) => Ev::UpdateFailed(String::new()),
                Err(e) => Ev::UpdateFailed(e),
            };
            let _ = proxy.send_event(ev);
        });
    }

    /// Klik "Cek pembaruan" di menu.
    fn check_update_now(&mut self) {
        if self.pending.lock().unwrap().is_some() {
            self.start_update(false);
            return;
        }
        let proxy = self.proxy.clone();
        let pending = self.pending.clone();
        thread::spawn(move || match updater::check() {
            Ok(Some(m)) => {
                let v = m.version.clone();
                *pending.lock().unwrap() = Some(m);
                let _ = proxy.send_event(Ev::UpdateAvailable(v));
            }
            Ok(None) => notify(&format!("lodo sudah versi terbaru ({}).", updater::VERSION)),
            Err(e) => notify(&e),
        });
    }

    fn open_window(&mut self) {
        if let Some(child) = &mut self.window
            && matches!(child.try_wait(), Ok(None))
        {
            shared::poke(WINDOW_LOCK);
            return;
        }
        if let Ok(exe) = std::env::current_exe() {
            self.window = Command::new(exe).arg("--window").spawn().ok();
        }
    }

    fn refresh(&mut self) {
        let (mood, text) = shared::mood(&self.live.lock().unwrap());
        if let Some(t) = &mut self.tray {
            t.set(mood, &text);
        }
    }
}

impl ApplicationHandler<Ev> for TrayApp {
    fn new_events(&mut self, _el: &ActiveEventLoop, cause: StartCause) {
        // Di macOS ikon tray harus dibuat setelah event loop berjalan.
        if cause == StartCause::Init {
            self.tray = Tray::new();
            self.refresh();
            if self.open_on_start {
                self.open_window();
            }
        }
    }

    fn resumed(&mut self, _el: &ActiveEventLoop) {}

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, _ev: WindowEvent) {}

    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        match ev {
            Ev::Open => self.open_window(),
            Ev::Refresh => self.refresh(),
            Ev::UpdateAvailable(v) => {
                if let Some(t) = &self.tray {
                    t.update_item.set_text(format!("Perbarui ke versi {v}"));
                }
                notify(&format!("lodo versi {v} tersedia. Klik \"Perbarui\" di menu lodo."));
            }
            Ev::UpdateNow(open_window) => self.start_update(open_window),
            Ev::UpdateFailed(e) => {
                self.updating = false;
                if let Some(t) = &self.tray {
                    t.update_item.set_enabled(true);
                    t.update_item.set_text("Cek pembaruan");
                }
                if e.is_empty() {
                    notify(&format!("lodo sudah versi terbaru ({}).", updater::VERSION));
                } else {
                    notify(&format!("Pembaruan gagal: {e}"));
                }
            }
            Ev::Restart => {
                // Langsung keluar supaya versi baru bisa dibuka; jangan menunggu apa pun
                // di thread utama (dulu notifikasi di sini bisa macet dan menahan restart).
                // Helper memperbarui dirinya sendiri (balasan IPC-nya instan).
                helper::request_update();
                if let Some(c) = &mut self.window {
                    let _ = c.kill();
                }
                el.exit();
            }
            Ev::Menu(id) => {
                let Some(t) = &self.tray else { return };
                if id == t.open_id {
                    self.open_window();
                } else if id == t.recheck_id {
                    thread::spawn(helper::recheck);
                } else if id == *t.update_item.id() {
                    self.check_update_now();
                } else if id == t.quit_id {
                    if let Some(c) = &mut self.window {
                        let _ = c.kill();
                    }
                    el.exit();
                }
            }
        }
    }
}
