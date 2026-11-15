//! Proses tray: sangat ringan (tanpa jendela / GPU). Jendela pengaturan
//! dijalankan sebagai proses terpisah hanya saat dibuka, lalu selesai saat ditutup.

use crate::shared::{self, Live, TRAY_LOCK, WINDOW_LOCK};
use crate::tray::Tray;
use crate::helper;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::thread;
use tray_icon::menu::{MenuEvent, MenuId};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

enum Ev {
    Menu(MenuId),
    Open,
    Refresh,
}

struct TrayApp {
    tray: Option<Tray>,
    live: Arc<Mutex<Live>>,
    window: Option<Child>,
    open_on_start: bool,
}

pub fn run(open_window: bool) {
    // Hanya satu tray. Kalau sudah ada, minta tray itu membuka jendela.
    let Ok(listener) = TcpListener::bind(TRAY_LOCK) else {
        shared::poke(TRAY_LOCK);
        return;
    };

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
                let _ = proxy.send_event(Ev::Open);
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
    shared::spawn_poller(live.clone(), move |changes| {
        if !changes.is_empty() {
            let _ = notify_rust::Notification::new().summary("lodo").body(&changes.join("\n")).show();
        }
        let _ = proxy.send_event(Ev::Refresh);
    });

    let mut app = TrayApp { tray: None, live, window: None, open_on_start: open_window };
    let _ = event_loop.run_app(&mut app);
}

impl TrayApp {
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
            Ev::Menu(id) => {
                let Some(t) = &self.tray else { return };
                if id == t.open_id {
                    self.open_window();
                } else if id == t.recheck_id {
                    thread::spawn(helper::recheck);
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
