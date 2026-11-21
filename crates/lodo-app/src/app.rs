use crate::helper;
use crate::shared::{self, Live};
use crate::tray::Mood;
use crate::updater;
use eframe::egui::{self, Color32, Key, RichText, ViewportCommand};
use lodo_core::peers::{self, Peer};
use lodo_core::update::Manifest;
use lodo_core::webscan::{self, WebApp};
use lodo_core::{domain, probe, Config, Route, Server, ServerStatus, VpnTarget};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------- token desain
//
// Satu skala untuk seluruh jendela, supaya semua layar terasa satu keluarga.

/// Warna aksen (tombol utama, fokus, tautan). Cukup gelap untuk teks putih (4.6:1).
const ACCENT: Color32 = Color32::from_rgb(0, 113, 227);

/// Skala huruf.
const TEXT_HERO: f32 = 22.0;
const TEXT_TITLE: f32 = 20.0;
const TEXT_SECTION: f32 = 15.5;
const TEXT_BODY: f32 = 14.0;
const TEXT_SMALL: f32 = 12.0;
const TEXT_CODE: f32 = 12.5;

/// Skala jarak.
const GAP_XS: f32 = 4.0;
const GAP_S: f32 = 8.0;
const GAP_M: f32 = 12.0;
const GAP_L: f32 = 16.0;

/// Warna yang bergantung tema (terang/gelap). Teks status memakai nada yang lebih
/// gelap/terang dari warna titik supaya tetap terbaca (kontras ≥4.5:1).
struct Pal {
    muted: Color32,
    card: Color32,
    card_stroke: Color32,
    separator: Color32,
    green: Color32,
    blue: Color32,
    red: Color32,
    orange: Color32,
}

fn pal(ui: &egui::Ui) -> Pal {
    if ui.visuals().dark_mode {
        Pal {
            muted: Color32::from_gray(165),
            card: Color32::from_gray(36),
            card_stroke: Color32::from_gray(56),
            separator: Color32::from_gray(52),
            green: Color32::from_rgb(98, 214, 128),
            blue: Color32::from_rgb(110, 172, 255),
            red: Color32::from_rgb(255, 120, 110),
            orange: Color32::from_rgb(255, 176, 80),
        }
    } else {
        Pal {
            muted: Color32::from_gray(100),
            card: Color32::WHITE,
            card_stroke: Color32::from_gray(222),
            separator: Color32::from_gray(232),
            green: Color32::from_rgb(22, 124, 54),
            blue: Color32::from_rgb(0, 98, 204),
            red: Color32::from_rgb(192, 40, 28),
            orange: Color32::from_rgb(160, 86, 0),
        }
    }
}

/// Warna titik status (sama dengan ikon tray).
fn mood_color(m: Mood) -> Color32 {
    let [r, g, b] = m.rgb();
    Color32::from_rgb(r, g, b)
}

// ---------------------------------------------------------------- tugas latar

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

// ---------------------------------------------------------------- state

enum Screen {
    Main,
    Edit(Box<Editor>),
    Settings(Settings),
}

/// Kolom form yang bisa punya pesan error sendiri.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    HomeIp,
    Vpn,
    WebPort,
    Advanced,
}

struct Editor {
    index: Option<usize>,
    /// Isi saat form dibuka, untuk tahu apakah ada perubahan yang belum disimpan.
    original: Server,
    name: String,
    home_ip: String,
    vpn: Vec<VpnTarget>,
    port: String,
    aliases: String,
    web_port: String,
    extra_ports: String,
    https: bool,
    advanced: bool,
    /// Error yang menempel di kolomnya; `None` sebagai field = error umum (di footer).
    error: Option<(Option<Field>, String)>,
    peers: Option<Vec<Peer>>,
    peers_task: Option<Task<Vec<Peer>>>,
    test: Option<Vec<(String, String, bool)>>,
    test_task: Option<Task<Vec<(String, String, bool)>>>,
    scan: Option<WebScan>,
    scan_task: Option<Task<WebScan>>,
    /// Tampilkan daftar hasil "Find automatically" walau sudah ada pilihan.
    scan_list_open: bool,
    confirm_delete: bool,
    confirm_discard: bool,
}

/// Hasil "Find automatically" halaman web di server.
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

impl Settings {
    fn new(config: &Config) -> Self {
        Self { domain: config.domain.clone(), confirm_uninstall: false }
    }
}

pub struct App {
    live: Arc<Mutex<Live>>,
    config: Option<Config>,
    screen: Screen,
    message: Option<(bool, String)>,
    /// (isi pesan, waktu pertama tampil) untuk menutup pesan sukses otomatis.
    message_seen: Option<(String, f64)>,
    install_task: Option<Task<Result<(), String>>>,
    uninstall_task: Option<Task<Result<(), String>>>,
    trust_task: Option<Task<Result<(), String>>>,
    /// Kapan "Check now" diklik, untuk menampilkan indikator sebentar.
    check_started: Option<f64>,
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
            message_seen: None,
            install_task: None,
            uninstall_task: None,
            trust_task: None,
            check_started: None,
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
                self.message = Some((true, "Couldn't change the open-at-login setting.".into()));
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
        let p = pal(ui);
        let color = if err { p.red } else { p.green };
        egui::Frame::new()
            .fill(color.gamma_multiply(0.12))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::containers::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.add(egui::Label::new(RichText::new(&msg).color(color)).wrap());
                    },
                    |ui| {
                        // Hanya error yang perlu ditutup sendiri.
                        if err && ui.button("Close").clicked() {
                            self.message = None;
                        }
                    },
                );
            });
        ui.add_space(GAP_M);
    }

    fn mood(&self) -> (Mood, String) {
        shared::mood(&self.live.lock().unwrap())
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Muat config dari helper begitu helper tersedia.
        let helper_up = self.live.lock().unwrap().helper_version.is_some();
        if helper_up && self.config.is_none() {
            self.config = helper::get_config().ok();
            // Debug: LODO_DEBUG_SCREEN=edit1 membuka server pertama dan menjalankan "Find automatically".
            if std::env::var("LODO_DEBUG_SCREEN").as_deref() == Ok("edit1")
                && let Some(s) = self.config.as_ref().and_then(|c| c.servers.first())
            {
                let mut ed = Editor::new(ctx, Some(0), Some(s));
                let ips: Vec<std::net::IpAddr> = s.candidates().into_iter().map(|(_, ip)| ip).collect();
                ed.scan_task = Some(spawn_task(ctx, move || scan_web(&ips)));
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
        self.finish_tasks(&ctx);
        self.shortcuts(&ctx);

        let helper_version = self.live.lock().unwrap().helper_version.clone();
        let ready = helper_version.is_some() && self.config.is_some();

        // Form punya footer tetap (Save/Cancel selalu terlihat).
        if ready && matches!(self.screen, Screen::Edit(_)) {
            let mut screen = std::mem::replace(&mut self.screen, Screen::Main);
            let mut next = None;
            if let Screen::Edit(ed) = &mut screen {
                let p = pal(ui);
                egui::Panel::bottom("footer")
                    .resizable(false)
                    .show_separator_line(true)
                    .frame(
                        egui::Frame::new()
                            .fill(ui.visuals().panel_fill)
                            .inner_margin(egui::Margin { left: 16, right: 16, top: 10, bottom: 12 }),
                    )
                    .show(ui, |ui| {
                        next = self.edit_footer(ui, ed, &p);
                    });
            }
            self.screen = next.unwrap_or(screen);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(16, 14)))
            .show(ui, |ui| {
                let mut scroll = egui::ScrollArea::vertical().auto_shrink(false);
                // Debug: LODO_DEBUG_SCROLL=600 menggulir layar (untuk screenshot bagian bawah).
                if let Some(y) = std::env::var("LODO_DEBUG_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
                    scroll = scroll.vertical_scroll_offset(y);
                }
                scroll.show(ui, |ui| {
                    self.header(ui);
                    ui.add_space(GAP_L);
                    self.message_banner(ui);
                    match helper_version {
                        None => self.setup_screen(ui),
                        Some(v) => {
                            self.helper_update_card(ui, &v);
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

// ---------------------------------------------------------------- alur umum

impl App {
    /// Ambil hasil pekerjaan latar yang sudah selesai.
    fn finish_tasks(&mut self, ctx: &egui::Context) {
        if let Some(r) = take(&self.install_task) {
            self.install_task = None;
            match r {
                Ok(()) => {
                    self.message = Some((false, "All set. Now add your first server.".into()));
                    if !self.autostart {
                        self.set_autostart(true);
                    }
                    self.live.lock().unwrap().helper_version = helper::ping();
                    self.config = helper::get_config().ok();
                    if self.config.as_ref().is_some_and(|c| c.servers.is_empty()) {
                        self.screen = Screen::Edit(Box::new(Editor::new(ctx, None, None)));
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
                    self.message = Some((false, "Secure connection is on. Your addresses now open with https://".into()));
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
                    self.message = Some((false, "lodo was removed from this computer and its addresses were cleaned up.".into()));
                }
                Err(e) => self.message = Some((true, e)),
            }
        }
    }

    /// Pintasan keyboard: Cmd/Ctrl+N tambah server, Cmd/Ctrl+, pengaturan,
    /// Esc kembali, Enter simpan (di form).
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let Some(config) = self.config.clone() else { return };
        let (cmd_n, cmd_comma, esc, enter) = ctx.input(|i| {
            (
                i.modifiers.command && i.key_pressed(Key::N),
                i.modifiers.command && i.key_pressed(Key::Comma),
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Enter) && !i.modifiers.any(),
            )
        });
        // Jangan bentrok dengan menu/daftar pilihan yang sedang terbuka.
        let popup_open = egui::Popup::is_any_open(ctx);
        match &mut self.screen {
            Screen::Main => {
                if cmd_n {
                    self.screen = Screen::Edit(Box::new(Editor::new(ctx, None, None)));
                } else if cmd_comma {
                    self.screen = Screen::Settings(Settings::new(&config));
                }
            }
            Screen::Settings(_) => {
                if esc && !popup_open {
                    self.screen = Screen::Main;
                }
            }
            Screen::Edit(ed) => {
                if popup_open {
                    return;
                }
                if esc {
                    if ed.confirm_discard {
                        ed.confirm_discard = false;
                    } else if ed.is_dirty() {
                        ed.confirm_discard = true;
                    } else {
                        self.screen = Screen::Main;
                    }
                } else if enter && !ed.confirm_discard && !ed.confirm_delete {
                    let mut screen = std::mem::replace(&mut self.screen, Screen::Main);
                    let mut next = None;
                    if let Screen::Edit(ed) = &mut screen {
                        next = self.save_editor(ed);
                    }
                    self.screen = next.unwrap_or(screen);
                }
            }
        }
    }

    fn helper_update_card(&mut self, ui: &mut egui::Ui, version: &str) {
        if lodo_core::update::is_newer(APP_VERSION, version) && !self.helper_update_asked {
            // Helper bisa memperbarui dirinya tanpa izin admin.
            self.helper_update_asked = true;
            thread::spawn(helper::request_update);
        }
        if version == APP_VERSION || self.install_task.is_some() {
            return;
        }
        let ctx = ui.ctx().clone();
        card(ui, |ui| {
            section_title(ui, "lodo is updating its background service");
            muted(ui, "This usually finishes on its own within a minute. If this message doesn't go away, update it manually.");
            ui.add_space(GAP_XS);
            if ui.button("Update now").on_hover_text("Needs your computer password").clicked() {
                self.install_task = Some(spawn_task(&ctx, helper::install));
            }
        });
    }
}

// ---------------------------------------------------------------- layar

impl App {
    fn header(&mut self, ui: &mut egui::Ui) {
        let (mood, text) = self.mood();
        let color = mood_color(mood);
        let p = pal(ui);
        let text_color = match mood {
            Mood::Home => p.green,
            Mood::Vpn => p.blue,
            Mood::Down => p.red,
            Mood::Idle => p.muted,
        };
        let on_main = matches!(self.screen, Screen::Main) && self.config.is_some();
        let mut open_settings = false;
        let now = ui.input(|i| i.time);
        let checking = self.check_started.is_some_and(|t| now - t < 2.0);
        if checking {
            ui.ctx().request_repaint_after(Duration::from_millis(200));
        }
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            ui.label(RichText::new("lodo").size(TEXT_TITLE).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if on_main && ui.button("Settings").on_hover_text("Settings (⌘,)").clicked() {
                    open_settings = true;
                }
                // Status dalam bentuk "pil": titik + teks, jadi tidak bergantung warna saja.
                egui::Frame::new()
                    .fill(color.gamma_multiply(0.14))
                    .corner_radius(99.0)
                    .inner_margin(egui::Margin::symmetric(12, 5))
                    .show(ui, |ui| {
                        // Induknya kanan-ke-kiri: teks dulu, lalu titik.
                        ui.horizontal(|ui| {
                            if checking {
                                ui.label(RichText::new("Checking…").color(text_color).strong());
                                ui.add(egui::Spinner::new().size(12.0));
                            } else {
                                ui.label(RichText::new(text).color(text_color).strong());
                                dot(ui, color);
                            }
                        });
                    });
            });
        });
        if open_settings && let Some(c) = &self.config {
            self.screen = Screen::Settings(Settings::new(c));
        }
    }

    /// Baris judul halaman: tombol Back dan judul, sejajar di tengah.
    fn page_title(ui: &mut egui::Ui, title: &str) -> bool {
        let mut back = false;
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            if ui.button("Back").on_hover_text("Back (Esc)").clicked() {
                back = true;
            }
            ui.add_space(GAP_XS);
            ui.label(RichText::new(title).size(TEXT_TITLE).strong());
        });
        ui.add_space(GAP_M);
        back
    }

    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Welcome to lodo").size(TEXT_HERO).strong());
            muted(ui, "One address for your home server, at home or on the go.");
            ui.add_space(GAP_M);
            for (n, text) in [
                (1, "Give your home server a name, like git.home.arpa."),
                (2, "At home, lodo uses your home network automatically."),
                (3, "Away from home, lodo goes through your VPN (Netbird, Tailscale, and others)."),
            ] {
                numbered_row(ui, n, text);
            }
            ui.add_space(GAP_XS);
            ui.add(egui::Label::new("No more changing settings every time you move.").wrap());
            ui.add_space(GAP_M);
            if self.install_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for your password… check the window that popped up.");
                });
            } else {
                if primary(ui, "Get started").clicked() {
                    self.message = None;
                    self.install_task = Some(spawn_task(ui.ctx(), helper::install));
                }
                ui.add_space(GAP_XS);
                muted(ui, "lodo asks for your computer password once, so it can manage addresses.");
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
        let https_trusted = self.live.lock().unwrap().https_trusted;
        let p = pal(ui);
        let mut next = Screen::Main;

        // HTTPS lokal butuh sertifikat lodo dipercaya komputer ini (izin sekali).
        let wants_https = config.servers.iter().any(|s| s.https_target().is_some());
        if wants_https && https_trusted == Some(false) {
            card(ui, |ui| {
                section_title(ui, "Turn on secure connection (https://)");
                muted(ui, "So your browser opens lodo addresses without a warning. It only applies to your private addresses, not to websites.");
                ui.add_space(GAP_XS);
                if self.trust_task.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Waiting for your password…");
                    });
                } else if primary(ui, "Turn on").clicked() {
                    self.trust_task = Some(spawn_task(&ctx, helper::trust_ca));
                }
            });
        }

        if config.servers.is_empty() {
            card(ui, |ui| {
                section_title(ui, "Add your first server");
                muted(ui, "A server can be a git server, a NAS, or any computer at home you want to reach from anywhere.");
                ui.add_space(GAP_S);
                if primary(ui, "Add server").clicked() {
                    next = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
                }
            });
            return next;
        }

        // Satu daftar server, satu baris per server.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Servers").size(TEXT_SECTION).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add server").on_hover_text("Add a server (⌘N)").clicked() {
                    next = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
                }
                if ui.button("Check now").on_hover_text("Check the route to every server again").clicked() {
                    thread::spawn(helper::recheck);
                    self.check_started = Some(ui.input(|i| i.time));
                }
            });
        });
        ui.add_space(GAP_XS);
        card(ui, |ui| {
            for (i, s) in config.servers.iter().enumerate() {
                if i > 0 {
                    ui.add_space(GAP_S);
                    separator(ui, p.separator);
                    ui.add_space(GAP_S);
                }
                let fqdn = s.fqdn(&config.domain);
                let st = status.iter().find(|x| x.fqdn == fqdn);
                let web = !s.forwards().is_empty();
                let https = s.https_target().is_some() && https_trusted == Some(true);
                let url = format!("{}://{fqdn}", if https { "https" } else { "http" });
                egui::containers::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.horizontal_top(|ui| {
                            ui.add_space(2.0);
                            ui.vertical(|ui| {
                                ui.add_space(6.0);
                                dot(ui, route_dot(st));
                            });
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(&fqdn).strong());
                                let (text, tone) = route_line(st);
                                let color = match tone {
                                    Tone::Bad => p.red,
                                    _ => p.muted,
                                };
                                ui.add(egui::Label::new(RichText::new(text).size(TEXT_SMALL).color(color)).wrap());
                            });
                        });
                    },
                    |ui| {
                        // Kanan ke kiri: menu "⋯" dulu, lalu aksi utama.
                        ui.menu_button("···", |ui| {
                            if ui.button("Copy address").clicked() {
                                ctx.copy_text(fqdn.clone());
                                self.message = Some((false, format!("{fqdn} copied.")));
                                ui.close();
                            }
                            if web && ui.button("Copy web link").clicked() {
                                ctx.copy_text(url.clone());
                                self.message = Some((false, format!("{url} copied.")));
                                ui.close();
                            }
                            if ui.button("Edit…").clicked() {
                                next = Screen::Edit(Box::new(Editor::new(&ctx, Some(i), Some(s))));
                                ui.close();
                            }
                        })
                        .response
                        .on_hover_text("More");
                        if web {
                            if primary(ui, "Open").on_hover_text(format!("Open {url}")).clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(&url));
                            }
                        } else if ui.button("Copy").on_hover_text(format!("Copy {fqdn}")).clicked() {
                            ctx.copy_text(fqdn.clone());
                            self.message = Some((false, format!("{fqdn} copied. Paste it into ssh, git, or any app.")));
                        }
                    },
                );
            }
        });

        // Cara pakai, disembunyikan sampai dibutuhkan.
        if let Some(first) = config.servers.first() {
            let fqdn = first.fqdn(&config.domain);
            ui.add_space(GAP_XS);
            egui::CollapsingHeader::new("How do I use these addresses?").default_open(false).show(ui, |ui| {
                ui.add(egui::Label::new("Use the same address wherever you are; lodo picks the route.").wrap());
                ui.add_space(GAP_XS);
                let mut examples = Vec::new();
                if !first.forwards().is_empty() {
                    let https = first.https_target().is_some() && https_trusted == Some(true);
                    examples.push(("Browser", format!("{}://{fqdn}", if https { "https" } else { "http" })));
                }
                examples.push(("SSH", format!("ssh you@{fqdn}")));
                examples.push(("git", format!("git clone ssh://git@{fqdn}/owner/repo.git")));
                egui::Grid::new("examples").num_columns(3).spacing([GAP_M, GAP_S]).show(ui, |ui| {
                    for (label, text) in examples {
                        ui.label(RichText::new(label).color(p.muted));
                        ui.label(RichText::new(&text).monospace().size(TEXT_CODE));
                        if ui.button("Copy").clicked() {
                            ctx.copy_text(text.clone());
                            self.message = Some((false, "Copied.".into()));
                        }
                        ui.end_row();
                    }
                });
            });
        }
        next
    }

    fn edit_screen(&mut self, ui: &mut egui::Ui, mut ed: Box<Editor>) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else { return Screen::Main };
        let p = pal(ui);
        if let Some(found) = take(&ed.peers_task) {
            ed.peers = Some(found);
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
            ed.scan_list_open = ed.web_port.trim().is_empty();
            ed.scan = Some(s);
            ed.scan_task = None;
        }
        let mut next = None;

        if Self::page_title(ui, if ed.index.is_some() { "Edit server" } else { "Add server" }) {
            if ed.is_dirty() {
                ed.confirm_discard = true;
            } else {
                next = Some(Screen::Main);
            }
        }
        let host = if ed.name.trim().is_empty() { "name".to_string() } else { ed.name.trim().to_string() };

        // ---- 1. Nama
        card(ui, |ui| {
            step(ui, 1, "Name it", false);
            let r = ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text("e.g. git, nas, or home").desired_width(f32::INFINITY));
            if r.changed() {
                ed.name = sanitize_label(&ed.name);
                ed.clear_error(Field::Name);
            }
            if let Some(e) = ed.error_for(Field::Name) {
                field_error(ui, &p, e);
            } else if ed.name.is_empty() {
                muted(ui, "A short name without spaces. It becomes your server's address.");
            } else {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Address:").color(p.muted));
                    ui.label(RichText::new(format!("{}.{}", ed.name, config.domain)).strong());
                });
            }
        });

        // ---- 2. Lokasi
        card(ui, |ui| {
            step(ui, 2, "Where is the server?", false);
            label(ui, "At home");
            let r = ui.add(egui::TextEdit::singleline(&mut ed.home_ip).hint_text("e.g. 192.168.1.10").desired_width(f32::INFINITY));
            if r.changed() {
                ed.clear_error(Field::HomeIp);
            }
            let ip = ed.home_ip.trim().to_string();
            if let Some(e) = ed.error_for(Field::HomeIp) {
                field_error(ui, &p, e);
            } else if !ip.is_empty() && ip.parse::<std::net::IpAddr>().is_err() {
                field_error(ui, &p, "That IP address isn't complete yet. Example: 192.168.1.10");
            } else {
                muted(ui, "The server's IP address on your home Wi-Fi. It usually starts with 192.168 or 10.");
            }

            ui.add_space(GAP_M);
            label(ui, "Away from home (through a VPN)");
            let mut remove = None;
            for (i, v) in ed.vpn.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut v.label).hint_text("VPN name").desired_width(110.0));
                    let ip_width = (ui.available_width() - 90.0).max(120.0);
                    ui.add(egui::TextEdit::singleline(&mut v.ip).hint_text("VPN IP address").desired_width(ip_width));
                    if ui.button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                ed.vpn.remove(i);
            }
            if let Some(e) = ed.error_for(Field::Vpn) {
                field_error(ui, &p, e);
            }
            ui.horizontal_wrapped(|ui| {
                match &ed.peers {
                    None => {
                        ui.spinner();
                        ui.label(RichText::new("Looking for devices on Netbird/Tailscale…").color(p.muted));
                    }
                    Some(peers) if peers.is_empty() => {}
                    Some(peers) => {
                        let mut picked = None;
                        let prompt = if ed.vpn.is_empty() { "Pick your server from the VPN" } else { "Add another from the VPN" };
                        egui::ComboBox::from_id_salt("peers").selected_text(prompt).width(230.0).show_ui(ui, |ui| {
                            for peer in peers {
                                if ui.selectable_label(false, format!("{} ({}, {})", peer.name, peer.vpn, peer.ip)).clicked() {
                                    picked = Some(peer.clone());
                                }
                            }
                        });
                        if let Some(peer) = picked {
                            if ed.name.is_empty() {
                                ed.name = sanitize_label(&peer.name);
                            }
                            if !ed.vpn.iter().any(|v| v.ip == peer.ip) {
                                ed.vpn.push(VpnTarget { label: peer.vpn.into(), ip: peer.ip });
                            }
                            ed.clear_error(Field::Vpn);
                        }
                    }
                }
                if ui.button("Enter manually").clicked() {
                    ed.vpn.push(VpnTarget { label: "VPN".into(), ip: String::new() });
                }
            });
            match &ed.peers {
                Some(found) if found.is_empty() => {
                    muted(ui, "Netbird/Tailscale wasn't found on this computer. If you use another VPN, enter its IP address manually.")
                }
                _ if ed.vpn.is_empty() => muted(ui, "Leave this empty if you only use the server at home."),
                _ => {}
            }

            // Cek koneksi, di langkah yang sama dengan alamatnya.
            ui.add_space(GAP_M);
            ui.horizontal(|ui| {
                let testing = ed.test_task.is_some();
                if ui
                    .add_enabled(!testing, egui::Button::new("Check connection"))
                    .on_hover_text("Try reaching the server now through each route")
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
                    muted(ui, "Enter at least one valid IP address first.");
                }
                for (label, ip, ok) in results {
                    ui.horizontal(|ui| {
                        dot(ui, if *ok { mood_color(Mood::Home) } else { mood_color(Mood::Down) });
                        let text = if *ok { format!("Reachable via {label}") } else { format!("Can't reach it via {label}") };
                        ui.label(RichText::new(text).color(if *ok { p.green } else { p.red }));
                        ui.label(RichText::new(ip).size(TEXT_SMALL).color(p.muted));
                    });
                }
            }
        });

        // ---- 3. Halaman web
        card(ui, |ui| {
            step(ui, 3, "Web page", true);
            muted(ui, "For example Gitea, a NAS, or Home Assistant that you open in a browser.");
            ui.add_space(GAP_XS);
            let ips: Vec<std::net::IpAddr> = ed.to_server().candidates().into_iter().map(|(_, ip)| ip).collect();
            let selected = ed.web_port.trim().parse::<u16>().ok();
            let scanning = ed.scan_task.is_some();
            let selected_app = ed.scan.as_ref().and_then(|s| s.apps.iter().find(|a| Some(a.port) == selected).cloned());

            if let Some(app) = selected_app.filter(|_| !ed.scan_list_open) {
                // Sudah dipilih: tampilkan ringkas saja.
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(app_title(&app)).strong());
                    if ui.button("Change").clicked() {
                        ed.scan_list_open = true;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!scanning && !ips.is_empty(), egui::Button::new("Find automatically"))
                        .on_disabled_hover_text("Enter the server's IP address first (step 2)")
                        .clicked()
                    {
                        ed.scan = None;
                        ed.scan_task = Some(spawn_task(&ctx, move || scan_web(&ips)));
                    }
                    if scanning {
                        ui.spinner();
                        ui.label(RichText::new("Looking for web pages on the server…").color(p.muted));
                    }
                });
                if let Some(scan) = ed.scan.clone() {
                    if !scan.reached {
                        field_error(ui, &p, "Can't reach the server from here. Check the IP address, and make sure you're at home or your VPN is on.");
                    } else if scan.apps.is_empty() {
                        muted(ui, "No web pages found. If you know the port number, enter it below.");
                    } else {
                        muted(ui, "Pick the one to open at this address:");
                        for app in &scan.apps {
                            let is_sel = selected == Some(app.port);
                            if ui.selectable_label(is_sel, app_title(app)).clicked() {
                                ed.pick_web(app.port, scan.ssh);
                                ed.scan_list_open = false;
                                ed.clear_error(Field::WebPort);
                            }
                        }
                    }
                }
            }

            ui.add_space(GAP_S);
            ui.horizontal(|ui| {
                ui.label("Port");
                let r = ui.add(egui::TextEdit::singleline(&mut ed.web_port).hint_text("none").desired_width(90.0));
                if r.changed() {
                    ed.clear_error(Field::WebPort);
                }
            });
            let port = ed.web_port.trim().parse::<u16>().ok().filter(|&p| p != 0);
            if let Some(e) = ed.error_for(Field::WebPort) {
                field_error(ui, &p, e);
            } else if port == Some(443) {
                field_warning(
                    ui,
                    &p,
                    "443 is the https port. Enter the port the web page runs on at the server (e.g. 80, 3000, 8080), or use Find automatically. lodo adds https:// for you.",
                );
            } else if !ed.web_port.trim().is_empty() && port.is_none() {
                field_error(ui, &p, "A port is a number, e.g. 80 or 3000.");
            } else if port.is_some() {
                ui.checkbox(&mut ed.https, "Use a secure connection (https://)");
                let scheme = if ed.https { "https" } else { "http" };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Opens at:").color(p.muted));
                    ui.label(RichText::new(format!("{scheme}://{host}.{}", config.domain)).strong());
                });
            } else {
                muted(ui, "Leave empty if the server has no web page.");
            }
        });

        // ---- Lanjutan
        card(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new("Advanced settings").size(TEXT_SECTION).strong()).default_open(ed.advanced).show(ui, |ui| {
                ed.advanced = true;
                ui.add_space(GAP_XS);
                label(ui, "Test port");
                ui.add(egui::TextEdit::singleline(&mut ed.port).desired_width(90.0));
                muted(ui, "The port lodo tries to see whether the server is reachable. 22 is SSH.");
                ui.add_space(GAP_S);
                label(ui, "Other names");
                ui.add(egui::TextEdit::singleline(&mut ed.aliases).hint_text("e.g. gitea, code").desired_width(f32::INFINITY));
                muted(ui, "Separate with commas. They all point to the same server.");
                if !ed.web_port.trim().is_empty() {
                    ui.add_space(GAP_S);
                    label(ui, "Other ports");
                    ui.add(egui::TextEdit::singleline(&mut ed.extra_ports).hint_text("e.g. 2222, 5432").desired_width(f32::INFINITY));
                    let fwd: Vec<String> = ed.to_server().forwards().iter().map(|(p, _)| p.to_string()).collect();
                    muted(
                        ui,
                        &format!("With a web page set, only these ports connect: {}. Add any other ports you use.", fwd.join(", ")),
                    );
                }
                if let Some(e) = ed.error_for(Field::Advanced) {
                    field_error(ui, &p, e);
                }
            });
        });

        // ---- Hapus (terpisah dari Save)
        if let Some(i) = ed.index {
            let fqdn = format!("{}.{}", ed.original.name, config.domain);
            danger_card(ui, |ui| {
                section_title(ui, "Remove this server");
                if ed.confirm_delete {
                    ui.add(egui::Label::new(format!("Remove {fqdn}? Its address stops working on this computer.")).wrap());
                    ui.horizontal(|ui| {
                        if danger_button(ui, &p, "Remove server").clicked() {
                            let mut c = config.clone();
                            if i < c.servers.len() {
                                c.servers.remove(i);
                            }
                            match self.save_config(c) {
                                Ok(()) => {
                                    self.message = Some((false, format!("{fqdn} removed.")));
                                    next = Some(Screen::Main);
                                }
                                Err(e) => ed.error = Some((None, e)),
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            ed.confirm_delete = false;
                        }
                    });
                } else {
                    muted(ui, "lodo stops managing this address. The server itself isn't affected.");
                    ui.add_space(GAP_XS);
                    if danger_button(ui, &p, "Remove…").clicked() {
                        ed.confirm_delete = true;
                    }
                }
            });
        }
        next.unwrap_or(Screen::Edit(ed))
    }

    /// Footer tetap di form: error umum, konfirmasi buang perubahan, Cancel/Save.
    fn edit_footer(&mut self, ui: &mut egui::Ui, ed: &mut Editor, p: &Pal) -> Option<Screen> {
        let mut next = None;
        if let Some((None, e)) = &ed.error {
            ui.add(egui::Label::new(RichText::new(e).color(p.red)).wrap());
            ui.add_space(GAP_XS);
        }
        #[derive(PartialEq)]
        enum Act {
            None,
            Cancel,
            Discard,
            KeepEditing,
            Save,
        }
        let mut act = Act::None;
        let confirm = ed.confirm_discard;
        ui.horizontal(|ui| {
            ui.set_min_height(30.0);
            if confirm {
                ui.label("Discard your changes?");
            } else if ui.button("Cancel").on_hover_text("Cancel (Esc)").clicked() {
                act = Act::Cancel;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if confirm {
                    if danger_button(ui, p, "Discard").clicked() {
                        act = Act::Discard;
                    }
                    if ui.button("Keep editing").clicked() {
                        act = Act::KeepEditing;
                    }
                } else if primary(ui, "Save").on_hover_text("Save (Enter)").clicked() {
                    act = Act::Save;
                }
            });
        });
        match act {
            Act::Cancel if ed.is_dirty() => ed.confirm_discard = true,
            Act::Cancel | Act::Discard => next = Some(Screen::Main),
            Act::KeepEditing => ed.confirm_discard = false,
            Act::Save => next = self.save_editor(ed),
            Act::None => {}
        }
        next
    }

    /// Validasi lalu simpan. Error ditempel di kolom yang bersangkutan.
    fn save_editor(&mut self, ed: &mut Editor) -> Option<Screen> {
        let config = self.config.clone()?;
        if ed.name.trim().is_empty() {
            ed.error = Some((Some(Field::Name), "Give the server a name.".into()));
            return None;
        }
        if ed.home_ip.trim().is_empty() {
            ed.error = Some((Some(Field::HomeIp), "Enter the server's IP address at home.".into()));
            return None;
        }
        let mut c = config;
        let s = ed.to_server();
        let added = ed.index.is_none();
        let fqdn = s.fqdn(&c.domain);
        match ed.index {
            Some(i) if i < c.servers.len() => c.servers[i] = s,
            _ => c.servers.push(s),
        }
        match self.save_config(c) {
            Ok(()) => {
                let msg = if added { format!("{fqdn} added. lodo picks the best route automatically.") } else { format!("{fqdn} saved.") };
                self.message = Some((false, msg));
                Some(Screen::Main)
            }
            Err(e) => {
                ed.error = Some((field_of_error(&e), e));
                None
            }
        }
    }

    fn settings_screen(&mut self, ui: &mut egui::Ui, mut st: Settings) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else { return Screen::Main };
        let p = pal(ui);
        let mut next = None;

        if Self::page_title(ui, "Settings") {
            next = Some(Screen::Main);
        }

        card(ui, |ui| {
            section_title(ui, "Address ending");
            muted(ui, "The last part of all your server addresses. Leave it as is if unsure.");
            ui.add_space(GAP_XS);
            ui.add(egui::TextEdit::singleline(&mut st.domain).desired_width(f32::INFINITY));
            let d = st.domain.trim().trim_end_matches('.').to_ascii_lowercase();
            let check = domain::validate_suffix(&d);
            match &check {
                Ok(()) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Addresses look like").color(p.muted));
                        ui.label(RichText::new(format!("git.{d}")).strong());
                    });
                }
                Err(e) => field_error(ui, &p, e),
            }
            if check.is_ok() && d != config.domain {
                ui.add_space(GAP_XS);
                if primary(ui, "Save").clicked() {
                    let mut c = config.clone();
                    c.domain = d;
                    match self.save_config(c) {
                        Ok(()) => self.message = Some((false, "Address ending saved.".into())),
                        Err(e) => self.message = Some((true, e)),
                    }
                }
            }
        });

        card(ui, |ui| {
            section_title(ui, "Secure connection (https://)");
            let trusted = self.live.lock().unwrap().https_trusted;
            let with_https: Vec<String> =
                config.servers.iter().filter(|s| s.https_target().is_some()).map(|s| s.fqdn(&config.domain)).collect();
            match trusted {
                Some(true) => status_line(ui, mood_color(Mood::Home), "On. lodo addresses open with https:// without warnings."),
                Some(false) => {
                    status_line(ui, mood_color(Mood::Down), "Not on yet on this computer.");
                    ui.add_space(GAP_XS);
                    if self.trust_task.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Waiting for your password…");
                        });
                    } else if primary(ui, "Turn on").clicked() {
                        self.trust_task = Some(spawn_task(&ctx, helper::trust_ca));
                    }
                }
                None => muted(ui, "Not in use yet. It turns on when a server has a web page."),
            }
            if !with_https.is_empty() {
                muted(ui, &format!("Used by {}.", with_https.join(", ")));
            }
            muted(ui, "Only applies to lodo's private addresses, not to websites.");
        });

        card(ui, |ui| {
            section_title(ui, "Startup");
            let mut on = self.autostart;
            if ui.checkbox(&mut on, "Open lodo when the computer starts").changed() {
                self.set_autostart(on);
            }
            muted(ui, "Address switching keeps working even when this window is closed.");
        });

        card(ui, |ui| {
            section_title(ui, "Updates");
            let mut on = self.auto_update;
            if ui.checkbox(&mut on, "Update automatically").changed() {
                match updater::set_auto_update(on) {
                    Ok(()) => self.auto_update = on,
                    Err(e) => self.message = Some((true, e)),
                }
            }
            ui.add_space(GAP_XS);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Version {APP_VERSION}")).color(p.muted));
                if self.updating {
                    ui.spinner();
                    ui.label("Updating… lodo will reopen in a moment.");
                } else if self.update_task.is_some() {
                    ui.spinner();
                    ui.label("Checking…");
                } else {
                    match &self.update_result {
                        Some(Ok(Some(m))) => {
                            ui.label(RichText::new(format!("Version {} is available.", m.version)).color(p.green));
                            if updater::can_self_install() {
                                if primary(ui, "Update now").clicked() {
                                    if shared::send_line(shared::TRAY_LOCK, "update") {
                                        self.updating = true;
                                    } else {
                                        self.message = Some((true, "The lodo menu bar icon isn't running. Open lodo again and retry.".into()));
                                    }
                                }
                            } else if ui.button("Open download page").clicked() {
                                ctx.open_url(egui::OpenUrl::new_tab(updater::download_page()));
                            }
                        }
                        Some(Ok(None)) => {
                            ui.label(RichText::new("You're up to date.").color(p.muted));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e.as_str()).color(p.red));
                        }
                        None => {}
                    }
                    if !matches!(self.update_result, Some(Ok(Some(_)))) && ui.button("Check for updates").clicked() {
                        self.update_task = Some(spawn_task(&ctx, updater::check));
                    }
                }
            });
        });

        danger_card(ui, |ui| {
            section_title(ui, "Remove lodo");
            if self.uninstall_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Waiting for your password…");
                });
            } else if st.confirm_uninstall {
                ui.add(
                    egui::Label::new(
                        "Remove lodo from this computer? Your server addresses stop working here. You'll be asked for your password.",
                    )
                    .wrap(),
                );
                ui.horizontal(|ui| {
                    if danger_button(ui, &p, "Remove lodo").clicked() {
                        self.uninstall_task = Some(spawn_task(&ctx, helper::uninstall));
                        st.confirm_uninstall = false;
                    }
                    if ui.button("Cancel").clicked() {
                        st.confirm_uninstall = false;
                    }
                });
            } else {
                muted(ui, "Removes lodo and all its addresses from this computer. Your servers aren't affected.");
                ui.add_space(GAP_XS);
                if danger_button(ui, &p, "Remove…").clicked() {
                    st.confirm_uninstall = true;
                }
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
        let mut ed = Self {
            index,
            original: s.clone().unwrap_or_else(|| Server {
                name: String::new(),
                home_ip: String::new(),
                vpn: Vec::new(),
                port: lodo_core::model::DEFAULT_PORT,
                aliases: Vec::new(),
                web_port: None,
                extra_ports: Vec::new(),
                https: true,
            }),
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
            scan_list_open: false,
            confirm_delete: false,
            confirm_discard: false,
        };
        // Patokan "belum diubah" memakai bentuk yang sama dengan hasil to_server().
        if index.is_some() {
            ed.original = ed.to_server();
        }
        ed
    }

    /// Ada perubahan yang belum disimpan?
    fn is_dirty(&self) -> bool {
        self.to_server() != self.original
    }

    fn error_for(&self, f: Field) -> Option<&str> {
        match &self.error {
            Some((Some(g), e)) if *g == f => Some(e),
            _ => None,
        }
    }

    fn clear_error(&mut self, f: Field) {
        if matches!(self.error, Some((Some(g), _)) if g == f) {
            self.error = None;
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

/// Tebak kolom mana yang dimaksud pesan validasi dari lodo-core.
fn field_of_error(e: &str) -> Option<Field> {
    let l = e.to_ascii_lowercase();
    if l.contains("vpn ip") {
        Some(Field::Vpn)
    } else if l.contains("home ip") {
        Some(Field::HomeIp)
    } else if l.contains("port") {
        Some(Field::WebPort)
    } else if l.contains("name") {
        Some(Field::Name)
    } else {
        None
    }
}

// ---------------------------------------------------------------- bantuan

fn route_label(r: &Route) -> String {
    match r {
        Route::Home => "home network".into(),
        Route::Vpn(v) => v.clone(),
        Route::Unreachable => "—".into(),
        Route::Unknown => "?".into(),
    }
}

enum Tone {
    Ok,
    Bad,
    Neutral,
}

/// Kalimat jalur di bawah nama server, mis. "Home network · 10.0.20.69".
fn route_line(st: Option<&ServerStatus>) -> (String, Tone) {
    match st {
        Some(ServerStatus { route: Route::Home, ip, .. }) => (with_ip("Home network", ip), Tone::Ok),
        Some(ServerStatus { route: Route::Vpn(v), ip, .. }) => (with_ip(&format!("Through {v}"), ip), Tone::Ok),
        Some(ServerStatus { route: Route::Unreachable, .. }) => ("Can't reach it. Check your Wi-Fi or turn on your VPN.".into(), Tone::Bad),
        _ => ("Checking…".into(), Tone::Neutral),
    }
}

fn with_ip(text: &str, ip: &Option<String>) -> String {
    match ip {
        Some(ip) => format!("{text} · {ip}"),
        None => text.into(),
    }
}

fn route_dot(st: Option<&ServerStatus>) -> Color32 {
    mood_color(match st.map(|s| &s.route) {
        Some(Route::Home) => Mood::Home,
        Some(Route::Vpn(_)) => Mood::Vpn,
        Some(Route::Unreachable) => Mood::Down,
        _ => Mood::Idle,
    })
}

fn app_title(app: &WebApp) -> String {
    if app.title.is_empty() { format!("Web page on port {}", app.port) } else { format!("{} · port {}", app.title, app.port) }
}

/// Tombol utama (aksi yang paling mungkin diinginkan di layar itu).
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(ACCENT)
            .stroke(egui::Stroke::NONE),
    )
}

/// Tombol untuk aksi yang menghapus sesuatu.
fn danger_button(ui: &mut egui::Ui, p: &Pal, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(p.red)))
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(TEXT_SECTION).strong());
}

/// Label kecil di atas kolom isian.
fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(TEXT_SMALL).strong());
}

/// Teks bantuan (abu-abu, cukup kontras, boleh terlipat).
fn muted(ui: &mut egui::Ui, text: &str) {
    let c = pal(ui).muted;
    ui.add(egui::Label::new(RichText::new(text).color(c)).wrap());
}

fn field_error(ui: &mut egui::Ui, p: &Pal, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(p.red)).wrap());
}

fn field_warning(ui: &mut egui::Ui, p: &Pal, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(p.orange)).wrap());
}

/// Titik status + kalimat, supaya arti tidak hanya dari warna.
fn status_line(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.add_space(4.0);
            dot(ui, color);
        });
        ui.add(egui::Label::new(text).wrap());
    });
}

/// Lingkaran bernomor (digambar, tidak bergantung font).
fn number(ui: &mut egui::Ui, n: u8) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 10.0, ACCENT);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n.to_string(), egui::FontId::proportional(12.0), Color32::WHITE);
}

/// Baris bernomor dengan teks yang boleh terlipat ke baris berikutnya.
fn numbered_row(ui: &mut egui::Ui, n: u8, text: &str) {
    let width = ui.available_width();
    ui.allocate_ui_with_layout(egui::vec2(width, 0.0), egui::Layout::left_to_right(egui::Align::TOP), |ui| {
        number(ui, n);
        let rest = ui.available_width();
        ui.allocate_ui_with_layout(egui::vec2(rest, 0.0), egui::Layout::top_down(egui::Align::LEFT), |ui| {
            ui.add_space(1.0);
            ui.add(egui::Label::new(text).wrap());
        });
    });
}

/// Judul langkah di form: nomor + teks (+ tanda "Optional").
fn step(ui: &mut egui::Ui, n: u8, text: &str, optional: bool) {
    let m = pal(ui).muted;
    ui.horizontal(|ui| {
        number(ui, n);
        ui.label(RichText::new(text).size(TEXT_SECTION).strong());
        if optional {
            ui.label(RichText::new("Optional").size(TEXT_SMALL).color(m));
        }
    });
    ui.add_space(GAP_XS);
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
    ui.painter().circle_filled(rect.center(), 4.5, color);
}

fn separator(ui: &mut egui::Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, color);
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = pal(ui);
    framed(ui, p.card, p.card_stroke, add)
}

/// Kartu untuk aksi berbahaya (hapus), dengan garis tepi bernada merah.
fn danger_card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = pal(ui);
    framed(ui, p.card, p.red.gamma_multiply(0.35), add)
}

fn framed<R>(ui: &mut egui::Ui, fill: Color32, stroke: Color32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke))
        .corner_radius(10.0)
        .inner_margin(GAP_L)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            add(ui)
        })
        .inner;
    ui.add_space(GAP_M);
    r
}

fn style(ctx: &egui::Context) {
    use egui::{FontId, Stroke, TextStyle, Theme};
    // Debug: LODO_DEBUG_THEME=dark|light memaksa tema (untuk screenshot).
    match std::env::var("LODO_DEBUG_THEME").as_deref() {
        Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
        Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
        _ => {}
    }
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::proportional(TEXT_BODY));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(TEXT_BODY));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(TEXT_TITLE));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(TEXT_SMALL));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(TEXT_CODE));
        s.spacing.item_spacing = egui::vec2(GAP_S, GAP_S);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        let radius = egui::CornerRadius::same(7);
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
        s.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.28);
        // Fokus kolom isian dan pilihan: garis aksen yang jelas.
        s.visuals.selection.stroke = Stroke::new(1.5, ACCENT);
    });
    // Kolom isian dan tombol kedua punya bingkai tipis, seperti app sistem.
    ctx.style_mut_of(Theme::Light, |s| {
        s.visuals.panel_fill = Color32::from_gray(246);
        s.visuals.text_edit_bg_color = Some(Color32::WHITE);
        s.visuals.weak_text_color = Some(Color32::from_gray(100));
        s.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(200));
        s.visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(250);
        s.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(160));
        s.visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(240);
    });
    ctx.style_mut_of(Theme::Dark, |s| {
        s.visuals.panel_fill = Color32::from_gray(24);
        s.visuals.text_edit_bg_color = Some(Color32::from_gray(20));
        s.visuals.weak_text_color = Some(Color32::from_gray(165));
        s.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_gray(72));
        s.visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(44);
        s.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_gray(110));
        s.visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(54);
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
