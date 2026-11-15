use crate::helper;
use crate::shared::{self, Live};
use crate::tray::Mood;
use crate::updater;
use lodo_core::update::Manifest;
use eframe::egui::{self, Color32, RichText, ViewportCommand};
use lodo_core::peers::{self, Peer};
use lodo_core::webscan::{self, WebApp};
use lodo_core::{domain, probe, Config, Route, Server, ServerStatus, VpnTarget};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
const GREEN: Color32 = Color32::from_rgb(40, 160, 80);
const RED: Color32 = Color32::from_rgb(220, 60, 50);
const ORANGE: Color32 = Color32::from_rgb(215, 130, 20);

/// Hasil pekerjaan latar belakang (thread) yang ditunggu UI.
type Task<T> = Arc<Mutex<Option<T>>>;

fn spawn_task<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Task<T> {
    let task: Task<T> = Arc::new(Mutex::new(None));
    let (t, ctx) = (task.clone(), ctx.clone());
    thread::spawn(move || {
        let r = f();
        *t.lock().unwrap() = Some(r);
        ctx.request_repaint();
    });
    task
}

fn take<T>(task: &Option<Task<T>>) -> Option<T> {
    task.as_ref().and_then(|t| t.lock().unwrap().take())
}

enum Screen {
    Main,
    Edit(Box<Editor>),
    Settings(Settings),
}

struct Editor {
    index: Option<usize>,
    name: String,
    home_ip: String,
    vpn: Vec<VpnTarget>,
    port: String,
    aliases: String,
    web_port: String,
    extra_ports: String,
    https: bool,
    advanced: bool,
    error: Option<String>,
    peers: Option<Vec<Peer>>,
    peers_task: Option<Task<Vec<Peer>>>,
    test: Option<Vec<(String, String, bool)>>,
    test_task: Option<Task<Vec<(String, String, bool)>>>,
    scan: Option<WebScan>,
    scan_task: Option<Task<WebScan>>,
    confirm_delete: bool,
}

/// Hasil "Cari otomatis" halaman web di server.
#[derive(Clone)]
struct WebScan {
    /// Server menjawab di salah satu alamat IP.
    reached: bool,
    apps: Vec<WebApp>,
    /// Port 22 (SSH) terbuka.
    ssh: bool,
}

/// Cari halaman web di alamat pertama yang menjawab (rumah dulu, lalu VPN).
fn scan_web(ips: &[std::net::IpAddr]) -> WebScan {
    for &ip in ips {
        let ssh_check = thread::spawn(move || probe::reachable(ip, 22, Duration::from_millis(1200)));
        let apps = webscan::scan(ip, webscan::COMMON_PORTS);
        let ssh = ssh_check.join().unwrap_or(false);
        if ssh || !apps.is_empty() {
            return WebScan { reached: true, apps, ssh };
        }
    }
    WebScan { reached: false, apps: Vec::new(), ssh: false }
}

struct Settings {
    domain: String,
    confirm_uninstall: bool,
}

pub struct App {
    live: Arc<Mutex<Live>>,
    config: Option<Config>,
    screen: Screen,
    message: Option<(bool, String)>,
    install_task: Option<Task<Result<(), String>>>,
    uninstall_task: Option<Task<Result<(), String>>>,
    trust_task: Option<Task<Result<(), String>>>,
    /// (isi pesan, waktu pertama tampil) untuk menutup pesan sukses otomatis.
    message_seen: Option<(String, f64)>,
    autostart: bool,
    auto_update: bool,
    update_task: Option<Task<Result<Option<Manifest>, String>>>,
    update_result: Option<Result<Option<Manifest>, String>>,
    updating: bool,
    helper_update_asked: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, lock: TcpListener) -> Self {
        let ctx = cc.egui_ctx.clone();
        style(&ctx);

        let live = Arc::new(Mutex::new(Live::default()));
        {
            let ctx = ctx.clone();
            shared::spawn_poller(live.clone(), move |_| ctx.request_repaint());
        }
        // Tray (atau peluncuran kedua) minta jendela ini maju ke depan.
        {
            let ctx = ctx.clone();
            thread::spawn(move || {
                for conn in lock.incoming().flatten() {
                    let mut line = String::new();
                    let _ = BufReader::new(conn).read_line(&mut line);
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    ctx.request_repaint();
                }
            });
        }
        ctx.send_viewport_cmd(ViewportCommand::Focus);

        let screen = match std::env::var("LODO_DEBUG_SCREEN").as_deref() {
            Ok("edit") => Screen::Edit(Box::new(Editor::new(&ctx, None, None))),
            Ok("settings") => Screen::Settings(Settings { domain: "example.com".into(), confirm_uninstall: false }),
            _ => Screen::Main,
        };
        Self {
            live,
            config: None,
            screen,
            message: None,
            install_task: None,
            uninstall_task: None,
            trust_task: None,
            message_seen: None,
            autostart: shared::autolaunch().and_then(|a| a.is_enabled().ok()).unwrap_or(false),
            auto_update: updater::auto_update(),
            update_task: None,
            update_result: None,
            updating: false,
            helper_update_asked: false,
        }
    }

    fn save_config(&mut self, new: Config) -> Result<(), String> {
        new.validate()?;
        helper::set_config(&new)?;
        self.config = Some(new);
        Ok(())
    }

    fn set_autostart(&mut self, on: bool) {
        if let Some(a) = shared::autolaunch() {
            let r = if on { a.enable() } else { a.disable() };
            if r.is_ok() {
                self.autostart = on;
            } else {
                self.message = Some((true, "Tidak bisa mengubah pengaturan buka otomatis.".into()));
            }
        }
    }

    /// Pesan di atas layar. Pesan sukses hilang sendiri; pesan error menunggu ditutup.
    fn message_banner(&mut self, ui: &mut egui::Ui) {
        let Some((err, msg)) = self.message.clone() else {
            self.message_seen = None;
            return;
        };
        let now = ui.input(|i| i.time);
        match &self.message_seen {
            Some((m, since)) if *m == msg => {
                if !err && now - since > 6.0 {
                    self.message = None;
                    self.message_seen = None;
                    return;
                }
            }
            _ => self.message_seen = Some((msg.clone(), now)),
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        let color = if err { RED } else { GREEN };
        egui::Frame::new()
            .fill(color.gamma_multiply(0.14))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::containers::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.label(RichText::new(&msg).color(color));
                    },
                    |ui| {
                        if ui.small_button("Tutup").clicked() {
                            self.message = None;
                        }
                    },
                );
            });
        ui.add_space(6.0);
    }

    fn mood(&self) -> (Mood, String) {
        shared::mood(&self.live.lock().unwrap())
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Muat config dari helper begitu helper tersedia.
        let helper_up = self.live.lock().unwrap().helper_version.is_some();
        if helper_up && self.config.is_none() {
            self.config = helper::get_config().ok();
            // Debug: LODO_DEBUG_SCREEN=edit1 membuka server pertama dan menjalankan "Cari otomatis".
            if std::env::var("LODO_DEBUG_SCREEN").as_deref() == Ok("edit1")
                && let Some(s) = self.config.as_ref().and_then(|c| c.servers.first())
            {
                let mut ed = Editor::new(_ctx, Some(0), Some(s));
                let ips: Vec<std::net::IpAddr> = s.candidates().into_iter().map(|(_, ip)| ip).collect();
                ed.scan_task = Some(spawn_task(_ctx, move || scan_web(&ips)));
                self.screen = Screen::Edit(Box::new(ed));
            }
        }
        if !helper_up {
            self.config = None;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        debug_screenshot(&ctx);

        if let Some(r) = take(&self.install_task) {
            self.install_task = None;
            match r {
                Ok(()) => {
                    self.message = Some((false, "Helper terpasang. Sekarang tambahkan server pertamamu.".into()));
                    if !self.autostart {
                        self.set_autostart(true);
                    }
                    self.live.lock().unwrap().helper_version = helper::ping();
                    self.config = helper::get_config().ok();
                    if self.config.as_ref().is_some_and(|c| c.servers.is_empty()) {
                        self.screen = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
                    }
                }
                Err(e) => self.message = Some((true, e)),
            }
        }
        if let Some(r) = take(&self.trust_task) {
            self.trust_task = None;
            match r {
                Ok(()) => {
                    self.live.lock().unwrap().https_trusted = Some(true);
                    self.message = Some((false, "HTTPS aktif. Buka alamatnya dengan https://".into()));
                }
                Err(e) => self.message = Some((true, e)),
            }
        }
        if let Some(r) = take(&self.update_task) {
            self.update_task = None;
            self.update_result = Some(r);
        }
        if let Some(r) = take(&self.uninstall_task) {
            self.uninstall_task = None;
            match r {
                Ok(()) => {
                    self.set_autostart(false);
                    self.live.lock().unwrap().helper_version = None;
                    self.config = None;
                    self.screen = Screen::Main;
                    self.message = Some((false, "lodo sudah dihapus dari sistem dan file hosts dibersihkan.".into()));
                }
                Err(e) => self.message = Some((true, e)),
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
            // Debug: LODO_DEBUG_SCROLL=600 menggulir layar (untuk screenshot bagian bawah).
            if let Some(y) = std::env::var("LODO_DEBUG_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                scroll = scroll.vertical_scroll_offset(y);
            }
            scroll.show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                self.message_banner(ui);
                let helper_version = self.live.lock().unwrap().helper_version.clone();
                match helper_version {
                    None => self.setup_screen(ui),
                    Some(v) => {
                        if lodo_core::update::is_newer(APP_VERSION, &v) && !self.helper_update_asked {
                            // Helper bisa memperbarui dirinya tanpa izin admin.
                            self.helper_update_asked = true;
                            thread::spawn(helper::request_update);
                        }
                        if v != APP_VERSION && self.install_task.is_none() {
                            card(ui, |ui| {
                                ui.label(RichText::new("lodo sedang memperbarui bagian dalamnya").strong());
                                ui.label(RichText::new("Biasanya selesai sendiri dalam semenit. Kalau pesan ini tidak hilang, klik tombol di bawah.").weak());
                                if ui.button("Perbarui sekarang (perlu izin admin)").clicked() {
                                    self.install_task = Some(spawn_task(&ctx, helper::install));
                                }
                            });
                        }
                        let screen = std::mem::replace(&mut self.screen, Screen::Main);
                        self.screen = match screen {
                            Screen::Main => self.main_screen(ui),
                            Screen::Edit(ed) => self.edit_screen(ui, ed),
                            Screen::Settings(s) => self.settings_screen(ui, s),
                        };
                    }
                }
            });
        });
    }
}

// ---------------------------------------------------------------- layar

impl App {
    fn header(&self, ui: &mut egui::Ui) {
        let (mood, text) = self.mood();
        let [r, g, b] = mood.rgb();
        let color = Color32::from_rgb(r, g, b);
        ui.horizontal(|ui| {
            ui.heading(RichText::new("lodo").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Status dalam bentuk "pil" berwarna supaya langsung terbaca.
                egui::Frame::new()
                    .fill(color.gamma_multiply(0.16))
                    .corner_radius(99.0)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        // Tata letak induknya kanan-ke-kiri, jadi teks dulu baru titik.
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(text).color(color).strong());
                            dot(ui, color);
                        });
                    });
            });
        });
        ui.add_space(4.0);
    }

    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Selamat datang di lodo").size(21.0).strong());
            ui.label(RichText::new("Satu alamat untuk server rumahmu, di rumah maupun di luar.").weak());
            ui.add_space(8.0);
            for (n, text) in [
                (1, "Beri server rumahmu nama, misalnya git.home.arpa."),
                (2, "Di rumah, lodo otomatis memakai jaringan rumah."),
                (3, "Di luar, lodo otomatis lewat VPN (Netbird, Tailscale, dan lainnya)."),
            ] {
                ui.horizontal(|ui| {
                    number(ui, n);
                    ui.label(text);
                });
            }
            ui.add_space(4.0);
            ui.label("Kamu tidak perlu mengganti pengaturan lagi tiap pindah tempat.");
            ui.add_space(10.0);
            if self.install_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Menunggu izin admin… lihat jendela yang muncul.");
                });
            } else {
                if primary(ui, "Mulai").clicked() {
                    self.message = None;
                    self.install_task = Some(spawn_task(ui.ctx(), helper::install));
                }
                ui.label(RichText::new("lodo akan meminta password komputermu satu kali saja, untuk mengatur alamat.").weak());
            }
        });
    }

    fn main_screen(&mut self, ui: &mut egui::Ui) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else {
            ui.spinner();
            return Screen::Main;
        };
        let status = self.live.lock().unwrap().status.clone();
        let mut next = Screen::Main;

        if config.servers.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new("Tambahkan server pertamamu").size(18.0).strong());
                ui.label("Server bisa berupa git, NAS, atau komputer apa pun di rumah yang ingin kamu akses dari mana saja.");
                ui.add_space(6.0);
                if primary(ui, "+ Tambah server").clicked() {
                    next = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
                }
            });
        }

        // HTTPS lokal butuh sertifikat lodo dipercaya komputer ini (izin sekali).
        let https_trusted = self.live.lock().unwrap().https_trusted;
        let wants_https = config.servers.iter().any(|s| s.https_target().is_some());
        if wants_https && https_trusted == Some(false) {
            card(ui, |ui| {
                ui.label(RichText::new("Satu langkah lagi: aktifkan https://").strong());
                ui.label(
                    RichText::new("Supaya browser tidak menampilkan peringatan saat membuka alamat lodo. Hanya berlaku untuk alamat pribadimu, bukan situs internet.")
                        .weak(),
                );
                if self.trust_task.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Menunggu izin… lihat jendela yang muncul.");
                    });
                } else if primary(ui, "Aktifkan").clicked() {
                    self.trust_task = Some(spawn_task(&ctx, helper::trust_ca));
                }
            });
        }

        for (i, s) in config.servers.iter().enumerate() {
            let fqdn = s.fqdn(&config.domain);
            let st = status.iter().find(|x| x.fqdn == fqdn);
            let web = !s.forwards().is_empty();
            let https = s.https_target().is_some() && https_trusted == Some(true);
            let url = format!("{}://{fqdn}", if https { "https" } else { "http" });
            card(ui, |ui| {
                // Teks di kiri (boleh terlipat), tombol di kanan.
                egui::containers::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&fqdn).strong().size(17.0));
                            let (color, text, ip) = route_text(st);
                            ui.horizontal_wrapped(|ui| {
                                dot(ui, color);
                                ui.label(RichText::new(text).color(color));
                                if let Some(ip) = ip {
                                    ui.label(RichText::new(ip).weak().small());
                                }
                            });
                            if web {
                                ui.horizontal_wrapped(|ui| {
                                    if ui.link(&url).on_hover_text("Buka di browser").clicked() {
                                        ctx.open_url(egui::OpenUrl::new_tab(&url));
                                    }
                                    if https {
                                        ui.label(RichText::new("aman").small().color(GREEN));
                                    }
                                });
                            }
                        });
                    },
                    |ui| {
                        // Ditambahkan dari kanan ke kiri.
                        if ui.button("Ubah").clicked() {
                            next = Screen::Edit(Box::new(Editor::new(&ctx, Some(i), Some(s))));
                        }
                        if ui.button("Salin").on_hover_text("Salin alamat untuk ditempel di aplikasi lain").clicked() {
                            ctx.copy_text(fqdn.clone());
                            self.message = Some((false, format!("{fqdn} disalin. Tempel di git, ssh, atau aplikasi lain.")));
                        }
                        if web && primary(ui, "Buka").on_hover_text(format!("Buka {url} di browser")).clicked() {
                            ctx.open_url(egui::OpenUrl::new_tab(&url));
                        }
                    },
                );
            });
        }

        if !config.servers.is_empty() {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if primary(ui, "+ Tambah server").clicked() {
                    next = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
                }
                if ui.button("Cek ulang").on_hover_text("Periksa lagi jalur ke semua server sekarang").clicked() {
                    thread::spawn(helper::recheck);
                    self.message = Some((false, "Mengecek ulang…".into()));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Pengaturan").clicked() {
                        next = Screen::Settings(Settings { domain: config.domain.clone(), confirm_uninstall: false });
                    }
                });
            });
        } else {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("Pengaturan").clicked() {
                    next = Screen::Settings(Settings { domain: config.domain.clone(), confirm_uninstall: false });
                }
            });
        }

        if let Some(first) = config.servers.first() {
            let fqdn = first.fqdn(&config.domain);
            ui.add_space(12.0);
            ui.label(RichText::new("Cara pakai").strong());
            ui.label(RichText::new("Pakai alamat yang sama di mana pun kamu berada. lodo yang memilih jalurnya.").weak());
            let mut examples = Vec::new();
            if !first.forwards().is_empty() {
                let https = first.https_target().is_some() && https_trusted == Some(true);
                examples.push(("Di browser", format!("{}://{fqdn}", if https { "https" } else { "http" })));
            }
            examples.push(("Lewat SSH", format!("ssh user@{fqdn}")));
            examples.push(("Lewat git", format!("git clone ssh://git@{fqdn}/user/repo.git")));
            for (label, text) in examples {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(label).weak());
                    ui.label(RichText::new(&text).monospace().size(13.0));
                    if ui.small_button("Salin").clicked() {
                        ctx.copy_text(text.clone());
                        self.message = Some((false, "Disalin.".into()));
                    }
                });
            }
        }
        next
    }

    fn edit_screen(&mut self, ui: &mut egui::Ui, mut ed: Box<Editor>) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else { return Screen::Main };
        if let Some(p) = take(&ed.peers_task) {
            ed.peers = Some(p);
            ed.peers_task = None;
        }
        if let Some(t) = take(&ed.test_task) {
            ed.test = Some(t);
            ed.test_task = None;
        }
        if let Some(s) = take(&ed.scan_task) {
            // Kalau hanya ada satu halaman web dan belum diisi, langsung pilihkan.
            if ed.web_port.trim().is_empty() && s.apps.len() == 1 {
                ed.pick_web(s.apps[0].port, s.ssh);
            }
            ed.scan = Some(s);
            ed.scan_task = None;
        }
        let mut next = None;

        ui.horizontal(|ui| {
            if ui.button("Kembali").clicked() {
                next = Some(Screen::Main);
            }
            ui.label(RichText::new(if ed.index.is_some() { "Ubah server" } else { "Tambah server" }).size(19.0).strong());
        });
        ui.add_space(2.0);

        // ---- 1. Nama
        card(ui, |ui| {
            step(ui, 1, "Beri nama");
            let r = ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text("mis. git, nas, atau rumah").desired_width(260.0));
            if r.changed() {
                ed.name = sanitize_label(&ed.name);
            }
            if ed.name.is_empty() {
                ui.label(RichText::new("Nama pendek tanpa spasi. Nama ini menjadi alamat servermu.").weak());
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Alamatnya nanti:").weak());
                    ui.label(RichText::new(format!("{}.{}", ed.name, config.domain)).strong().color(ACCENT));
                });
            }
        });

        // ---- 2. Lokasi
        card(ui, |ui| {
            step(ui, 2, "Di mana servernya?");
            ui.label("Saat di rumah");
            ui.add(egui::TextEdit::singleline(&mut ed.home_ip).hint_text("mis. 192.168.1.10").desired_width(260.0));
            let ip = ed.home_ip.trim();
            if !ip.is_empty() && ip.parse::<std::net::IpAddr>().is_err() {
                ui.label(RichText::new("Alamat IP belum lengkap. Contoh: 192.168.1.10").color(ORANGE));
            } else {
                ui.label(RichText::new("Alamat IP server di WiFi rumah. Biasanya diawali 192.168 atau 10.").weak());
            }

            ui.add_space(6.0);
            ui.label("Saat di luar rumah (lewat VPN)");
            let mut remove = None;
            for (i, v) in ed.vpn.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut v.label).hint_text("Nama VPN").desired_width(90.0));
                    ui.add(egui::TextEdit::singleline(&mut v.ip).hint_text("Alamat IP VPN").desired_width(150.0));
                    if ui.small_button("Hapus").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                ed.vpn.remove(i);
            }
            ui.horizontal_wrapped(|ui| {
                match &ed.peers {
                    None => {
                        ui.spinner();
                        ui.label(RichText::new("Mencari perangkat di Netbird/Tailscale…").weak());
                    }
                    Some(peers) if peers.is_empty() => {}
                    Some(peers) => {
                        let mut picked = None;
                        egui::ComboBox::from_id_salt("peers")
                            .selected_text("Pilih servermu dari VPN…")
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for p in peers {
                                    if ui.selectable_label(false, format!("{} ({}, {})", p.name, p.vpn, p.ip)).clicked() {
                                        picked = Some(p.clone());
                                    }
                                }
                            });
                        if let Some(p) = picked {
                            if ed.name.is_empty() {
                                ed.name = sanitize_label(&p.name);
                            }
                            if !ed.vpn.iter().any(|v| v.ip == p.ip) {
                                ed.vpn.push(VpnTarget { label: p.vpn.into(), ip: p.ip });
                            }
                        }
                    }
                }
                if ui.button("+ Isi manual").clicked() {
                    ed.vpn.push(VpnTarget { label: "VPN".into(), ip: String::new() });
                }
            });
            let hint = match &ed.peers {
                Some(p) if p.is_empty() => "Netbird/Tailscale tidak ditemukan di komputer ini. Kalau pakai VPN lain, isi alamat IP-nya secara manual.",
                _ if ed.vpn.is_empty() => "Boleh dikosongkan kalau server hanya dipakai di rumah.",
                _ => "",
            };
            if !hint.is_empty() {
                ui.label(RichText::new(hint).weak());
            }
        });

        // ---- 3. Halaman web
        card(ui, |ui| {
            step(ui, 3, "Punya halaman web? (opsional)");
            ui.label(RichText::new("Misalnya Gitea, NAS, atau Home Assistant yang dibuka lewat browser.").weak());
            let host = if ed.name.trim().is_empty() { "nama".to_string() } else { ed.name.trim().to_string() };
            let ips: Vec<std::net::IpAddr> = ed.to_server().candidates().into_iter().map(|(_, ip)| ip).collect();
            ui.horizontal(|ui| {
                let scanning = ed.scan_task.is_some();
                if ui
                    .add_enabled(!scanning && !ips.is_empty(), egui::Button::new("Cari otomatis"))
                    .on_disabled_hover_text("Isi alamat IP server dulu (langkah 2)")
                    .clicked()
                {
                    ed.scan = None;
                    ed.scan_task = Some(spawn_task(&ctx, move || scan_web(&ips)));
                }
                if scanning {
                    ui.spinner();
                    ui.label(RichText::new("Mencari halaman web di server…").weak());
                }
            });
            if let Some(scan) = ed.scan.clone() {
                if !scan.reached {
                    ui.label(RichText::new("Server tidak terjangkau dari sini. Pastikan alamat IP benar, dan kamu sedang di rumah atau VPN menyala.").color(ORANGE));
                } else if scan.apps.is_empty() {
                    ui.label(RichText::new("Tidak ada halaman web yang ditemukan. Kalau tahu nomor port-nya, isi di bawah.").weak());
                } else {
                    ui.label(RichText::new("Ditemukan. Pilih yang ingin dibuka lewat alamat ini:").weak());
                    for app in &scan.apps {
                        let selected = ed.web_port.trim() == app.port.to_string();
                        let text = if app.title.is_empty() {
                            format!("Halaman web di port {}", app.port)
                        } else {
                            format!("{}  (port {})", app.title, app.port)
                        };
                        if ui.selectable_label(selected, text).clicked() {
                            ed.pick_web(app.port, scan.ssh);
                        }
                    }
                }
            }
            ui.horizontal(|ui| {
                ui.label("Nomor port");
                ui.add(egui::TextEdit::singleline(&mut ed.web_port).hint_text("kosong").desired_width(80.0));
            });
            let port = ed.web_port.trim().parse::<u16>().ok().filter(|&p| p != 0);
            if port == Some(443) {
                ui.label(
                    RichText::new("443 adalah port https. Isi port tempat halaman web berjalan di server (mis. 80, 3000, 8080), atau klik Cari otomatis. https:// dibuat otomatis oleh lodo.")
                        .color(ORANGE),
                );
            } else if !ed.web_port.trim().is_empty() && port.is_none() {
                ui.label(RichText::new("Nomor port berupa angka, mis. 80 atau 3000.").color(ORANGE));
            } else if port.is_some() {
                ui.checkbox(&mut ed.https, "Pakai https:// (koneksi aman)");
                let scheme = if ed.https { "https" } else { "http" };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Nanti dibuka di:").weak());
                    ui.label(RichText::new(format!("{scheme}://{host}.{}", config.domain)).strong().color(ACCENT));
                });
            } else {
                ui.label(RichText::new("Kosongkan kalau server tidak punya halaman web.").weak());
            }
        });

        egui::CollapsingHeader::new("Pengaturan lanjutan").default_open(ed.advanced).show(ui, |ui| {
            ed.advanced = true;
            egui::Grid::new("adv").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Port untuk tes");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut ed.port).desired_width(70.0));
                    ui.label(RichText::new("Port yang dicoba lodo untuk tahu server bisa dijangkau. 22 = SSH.").weak());
                });
                ui.end_row();
                ui.label("Nama lain");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut ed.aliases).hint_text("mis. gitea, code").desired_width(220.0));
                    ui.label(RichText::new("Pisahkan dengan koma. Semua mengarah ke server yang sama.").weak());
                });
                ui.end_row();
                if !ed.web_port.trim().is_empty() {
                    ui.label("Port lain");
                    ui.vertical(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut ed.extra_ports).hint_text("mis. 2222, 5432").desired_width(220.0));
                        let fwd: Vec<String> = ed.to_server().forwards().iter().map(|(p, _)| p.to_string()).collect();
                        ui.label(
                            RichText::new(format!(
                                "Kalau halaman web diisi, hanya port tertentu yang tersambung. Sekarang: {}. Tambahkan port lain yang kamu pakai.",
                                fwd.join(", ")
                            ))
                            .weak(),
                        );
                    });
                    ui.end_row();
                }
            });
        });

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let testing = ed.test_task.is_some();
            if ui
                .add_enabled(!testing, egui::Button::new("Tes koneksi"))
                .on_hover_text("Coba sambungkan ke server sekarang lewat tiap jalur")
                .clicked()
            {
                let s = ed.to_server();
                ed.test = None;
                ed.test_task = Some(spawn_task(&ctx, move || {
                    probe::probe_all(&s)
                        .into_iter()
                        .map(|(r, ip, ok)| (route_label(&r), ip.to_string(), ok))
                        .collect()
                }));
            }
            if testing {
                ui.spinner();
            }
        });
        if let Some(results) = &ed.test {
            if results.is_empty() {
                ui.label(RichText::new("Isi minimal satu alamat IP yang benar dulu.").weak());
            }
            for (label, ip, ok) in results {
                let (c, mark) = if *ok { (GREEN, "Tersambung") } else { (RED, "Tidak tersambung") };
                ui.horizontal(|ui| {
                    dot(ui, c);
                    ui.label(RichText::new(format!("{mark} lewat {label}")).color(c));
                    ui.label(RichText::new(ip).weak().small());
                });
            }
        }

        if let Some(e) = &ed.error {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(RED));
        }

        ui.add_space(8.0);
        let mut delete = false;
        ui.horizontal(|ui| {
            if primary(ui, "Simpan").clicked() {
                if ed.name.trim().is_empty() {
                    ed.error = Some("Beri nama server dulu (langkah 1).".into());
                } else if ed.home_ip.trim().is_empty() {
                    ed.error = Some("Isi alamat IP server saat di rumah (langkah 2).".into());
                } else {
                    let mut c = config.clone();
                    let s = ed.to_server();
                    match ed.index {
                        Some(i) if i < c.servers.len() => c.servers[i] = s,
                        _ => c.servers.push(s),
                    }
                    match self.save_config(c) {
                        Ok(()) => {
                            self.message = Some((false, "Tersimpan. lodo akan memilih jalur terbaik otomatis.".into()));
                            next = Some(Screen::Main);
                        }
                        Err(e) => ed.error = Some(e),
                    }
                }
            }
            if ui.button("Batal").clicked() {
                next = Some(Screen::Main);
            }
            if ed.index.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ed.confirm_delete {
                        if ui.button(RichText::new("Ya, hapus").color(RED)).clicked() {
                            delete = true;
                        }
                        if ui.button("Tidak jadi").clicked() {
                            ed.confirm_delete = false;
                        }
                        ui.label("Hapus server ini?");
                    } else if ui.button(RichText::new("Hapus server").color(RED)).clicked() {
                        ed.confirm_delete = true;
                    }
                });
            }
        });
        if delete && let Some(i) = ed.index {
            let mut c = config.clone();
            if i < c.servers.len() {
                c.servers.remove(i);
            }
            match self.save_config(c) {
                Ok(()) => {
                    self.message = Some((false, "Server dihapus.".into()));
                    next = Some(Screen::Main);
                }
                Err(e) => ed.error = Some(e),
            }
        }
        next.unwrap_or(Screen::Edit(ed))
    }

    fn settings_screen(&mut self, ui: &mut egui::Ui, mut st: Settings) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else { return Screen::Main };
        let mut next = None;

        ui.horizontal(|ui| {
            if ui.button("Kembali").clicked() {
                next = Some(Screen::Main);
            }
            ui.label(RichText::new("Pengaturan").size(19.0).strong());
        });
        ui.add_space(2.0);

        card(ui, |ui| {
            ui.label(RichText::new("Akhiran alamat").strong());
            ui.label(RichText::new("Bagian belakang semua alamat servermu. Biarkan apa adanya kalau ragu.").weak());
            ui.horizontal(|ui| {
                ui.label("server.");
                ui.add(egui::TextEdit::singleline(&mut st.domain).desired_width(200.0));
            });
            let d = st.domain.trim().trim_end_matches('.').to_ascii_lowercase();
            let check = domain::validate_suffix(&d);
            match &check {
                Ok(()) => ui.label(RichText::new(format!("Aman, tidak bentrok dengan internet. Contoh: git.{d}")).color(GREEN)),
                Err(e) => ui.label(RichText::new(e.as_str()).color(RED)),
            };
            ui.label(RichText::new("Rekomendasi: home.arpa (khusus jaringan rumah), internal, atau lan.").weak());
            let changed = d != config.domain;
            if ui.add_enabled(check.is_ok() && changed, egui::Button::new("Simpan akhiran")).clicked() {
                let mut c = config.clone();
                c.domain = d;
                match self.save_config(c) {
                    Ok(()) => self.message = Some((false, "Akhiran alamat diperbarui.".into())),
                    Err(e) => self.message = Some((true, e)),
                }
            }
        });

        card(ui, |ui| {
            ui.label(RichText::new("Koneksi aman (https://)").strong());
            let trusted = self.live.lock().unwrap().https_trusted;
            let with_https: Vec<String> =
                config.servers.iter().filter(|s| s.https_target().is_some()).map(|s| s.fqdn(&config.domain)).collect();
            match trusted {
                Some(true) => {
                    ui.horizontal_wrapped(|ui| {
                        dot(ui, GREEN);
                        ui.label("Aktif. Alamat lodo bisa dibuka dengan https:// tanpa peringatan.");
                    });
                }
                Some(false) => {
                    ui.horizontal_wrapped(|ui| {
                        dot(ui, Color32::from_rgb(230, 150, 30));
                        ui.label("Belum aktif di komputer ini.");
                    });
                    if self.trust_task.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Menunggu izin…");
                        });
                    } else if primary(ui, "Aktifkan").clicked() {
                        self.trust_task = Some(spawn_task(&ctx, helper::trust_ca));
                    }
                }
                None => {
                    ui.label(RichText::new("Belum dipakai. Isi halaman web di salah satu server (langkah 3 saat menambah server).").weak());
                }
            }
            if !with_https.is_empty() {
                ui.label(RichText::new(format!("Dipakai oleh: {}", with_https.join(", "))).weak());
            }
            ui.label(
                RichText::new("Hanya berlaku untuk alamat pribadi lodo (mis. .home.arpa), tidak memengaruhi situs internet.")
                    .weak(),
            );
        });

        card(ui, |ui| {
            let mut on = self.autostart;
            if ui.checkbox(&mut on, "Buka lodo otomatis saat komputer menyala").changed() {
                self.set_autostart(on);
            }
            ui.label(RichText::new("Pergantian alamat tetap berjalan walau aplikasi ini ditutup.").weak());
        });

        card(ui, |ui| {
            ui.label(RichText::new("Pembaruan").strong());
            let mut on = self.auto_update;
            if ui.checkbox(&mut on, "Perbarui otomatis").changed() {
                match updater::set_auto_update(on) {
                    Ok(()) => self.auto_update = on,
                    Err(e) => self.message = Some((true, e)),
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Versi {APP_VERSION}.")).weak());
                if self.updating {
                    ui.spinner();
                    ui.label("Memperbarui… lodo akan terbuka lagi sebentar.");
                } else if self.update_task.is_some() {
                    ui.spinner();
                    ui.label("Memeriksa…");
                } else {
                    match &self.update_result {
                        Some(Ok(Some(m))) => {
                            ui.label(RichText::new(format!("Versi {} tersedia.", m.version)).color(GREEN));
                            if updater::can_self_install() {
                                if ui.button("Perbarui sekarang").clicked() {
                                    if shared::send_line(shared::TRAY_LOCK, "update") {
                                        self.updating = true;
                                    } else {
                                        self.message = Some((true, "Ikon lodo di menu bar tidak berjalan. Buka lodo lagi lalu coba ulang.".into()));
                                    }
                                }
                            } else if ui.button("Buka halaman unduhan").clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(updater::download_page()));
                            }
                        }
                        Some(Ok(None)) => {
                            ui.label("Sudah versi terbaru.");
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e.as_str()).color(RED));
                        }
                        None => {}
                    }
                    if !matches!(self.update_result, Some(Ok(Some(_)))) && ui.button("Cek pembaruan").clicked() {
                        self.update_task = Some(spawn_task(&ctx, updater::check));
                    }
                }
            });
        });

        card(ui, |ui| {
            ui.label(RichText::new("Hapus lodo").strong());
            ui.label(RichText::new("Menghapus lodo dan semua alamatnya dari komputer ini. Servermu tidak terpengaruh.").weak());
            if self.uninstall_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Menunggu izin admin…");
                });
            } else if st.confirm_uninstall {
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Ya, hapus").color(RED)).clicked() {
                        self.uninstall_task = Some(spawn_task(&ctx, helper::uninstall));
                        st.confirm_uninstall = false;
                    }
                    if ui.button("Batal").clicked() {
                        st.confirm_uninstall = false;
                    }
                });
            } else if ui.button(RichText::new("Hapus lodo dari komputer ini…").color(RED)).clicked() {
                st.confirm_uninstall = true;
            }
        });

        next.unwrap_or(Screen::Settings(st))
    }
}

// ---------------------------------------------------------------- editor

impl Editor {
    fn new(ctx: &egui::Context, index: Option<usize>, s: Option<&Server>) -> Self {
        let peers_task = Some(spawn_task(ctx, peers::detect));
        let s = s.cloned();
        Self {
            index,
            name: s.as_ref().map(|s| s.name.clone()).unwrap_or_default(),
            home_ip: s.as_ref().map(|s| s.home_ip.clone()).unwrap_or_default(),
            vpn: s.as_ref().map(|s| s.vpn.clone()).unwrap_or_default(),
            port: s.as_ref().map(|s| s.port).unwrap_or(lodo_core::model::DEFAULT_PORT).to_string(),
            aliases: s.as_ref().map(|s| s.aliases.join(", ")).unwrap_or_default(),
            web_port: s.as_ref().and_then(|s| s.web_port).map(|p| p.to_string()).unwrap_or_default(),
            https: s.as_ref().is_none_or(|s| s.https),
            extra_ports: s.as_ref().map(|s| s.extra_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
            advanced: false,
            error: None,
            peers: None,
            peers_task,
            test: None,
            test_task: None,
            scan: None,
            scan_task: None,
            confirm_delete: false,
        }
    }

    /// Pilih halaman web di `port`. Kalau SSH tidak ada dan port tes masih bawaan,
    /// pakai port web juga untuk tes, supaya server tidak dianggap mati.
    fn pick_web(&mut self, port: u16, ssh: bool) {
        self.web_port = port.to_string();
        if !ssh && self.port.trim() == lodo_core::model::DEFAULT_PORT.to_string() {
            self.port = port.to_string();
        }
    }

    fn to_server(&self) -> Server {
        Server {
            name: self.name.trim().to_string(),
            home_ip: self.home_ip.trim().to_string(),
            vpn: self
                .vpn
                .iter()
                .filter(|v| !v.ip.trim().is_empty())
                .map(|v| VpnTarget {
                    label: if v.label.trim().is_empty() { "VPN".into() } else { v.label.trim().into() },
                    ip: v.ip.trim().into(),
                })
                .collect(),
            port: self.port.trim().parse().unwrap_or(0),
            aliases: self.aliases.split(',').map(|a| a.trim().to_ascii_lowercase()).filter(|a| !a.is_empty()).collect(),
            // Isian tidak valid jadi 0 supaya ditolak validasi dengan pesan yang jelas.
            web_port: Some(self.web_port.trim()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0)),
            https: self.https,
            extra_ports: self
                .extra_ports
                .split([',', ' '])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| p.parse().unwrap_or(0))
                .collect(),
        }
    }
}

// ---------------------------------------------------------------- bantuan

fn route_label(r: &Route) -> String {
    match r {
        Route::Home => "jaringan rumah".into(),
        Route::Vpn(v) => v.clone(),
        Route::Unreachable => "—".into(),
        Route::Unknown => "?".into(),
    }
}

/// (warna, kalimat status, alamat IP yang sedang dipakai)
fn route_text(st: Option<&ServerStatus>) -> (Color32, String, Option<String>) {
    let rgb = |m: Mood| {
        let [r, g, b] = m.rgb();
        Color32::from_rgb(r, g, b)
    };
    match st {
        Some(ServerStatus { route: Route::Home, ip, .. }) => (rgb(Mood::Home), "Tersambung lewat jaringan rumah".into(), ip.clone()),
        Some(ServerStatus { route: Route::Vpn(v), ip, .. }) => (rgb(Mood::Vpn), format!("Tersambung lewat {v}"), ip.clone()),
        Some(ServerStatus { route: Route::Unreachable, .. }) => {
            (rgb(Mood::Down), "Tidak tersambung. Cek WiFi atau nyalakan VPN.".into(), None)
        }
        _ => (rgb(Mood::Idle), "Sedang mengecek…".into(), None),
    }
}

/// Tombol utama (aksi yang paling mungkin diinginkan di layar itu).
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE).strong()).fill(ACCENT))
}

/// Lingkaran bernomor (digambar, tidak bergantung font).
fn number(ui: &mut egui::Ui, n: u8) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 11.0, ACCENT);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        n.to_string(),
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
}

/// Judul langkah di form: nomor + teks.
fn step(ui: &mut egui::Ui, n: u8, text: &str) {
    ui.horizontal(|ui| {
        number(ui, n);
        ui.label(RichText::new(text).size(16.0).strong());
    });
}

/// Nama hanya boleh huruf kecil, angka, dan tanda minus.
fn sanitize_label(s: &str) -> String {
    s.to_ascii_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' || c == '.' { '-' } else { c })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .take(63)
        .collect()
}

/// Titik warna kecil (digambar, tidak bergantung font).
fn dot(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let dark = ui.visuals().dark_mode;
    let r = egui::Frame::new()
        .fill(if dark { Color32::from_gray(38) } else { Color32::WHITE })
        .stroke(egui::Stroke::new(1.0, if dark { Color32::from_gray(58) } else { Color32::from_gray(226) }))
        .corner_radius(12.0)
        .inner_margin(14.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(6.0);
    r
}

fn style(ctx: &egui::Context) {
    use egui::{FontId, TextStyle};
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        let radius = egui::CornerRadius::same(8);
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        s.visuals.hyperlink_color = ACCENT;
        s.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    });
}

/// Khusus pengembangan: `LODO_SCREENSHOT=/path/prefix` menyimpan screenshot jendela
/// (format PPM) tiap 3 detik, untuk mengecek tampilan tanpa izin screen recording.
fn debug_screenshot(ctx: &egui::Context) {
    let Some(prefix) = std::env::var_os("LODO_SCREENSHOT") else { return };
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    static N: AtomicU64 = AtomicU64::new(0);
    let now = ctx.input(|i| i.time) as u64;
    ctx.request_repaint_after(Duration::from_millis(500));
    if now < LAST.load(Ordering::Relaxed) + 3 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    ctx.request_screenshot(move |image| {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut data = format!("P6\n{} {}\n255\n", image.size[0], image.size[1]).into_bytes();
        for p in &image.pixels {
            data.extend_from_slice(&[p.r(), p.g(), p.b()]);
        }
        let mut path = prefix.clone();
        path.push(format!("-{n}.ppm"));
        let _ = std::fs::write(path, data);
    });
}
