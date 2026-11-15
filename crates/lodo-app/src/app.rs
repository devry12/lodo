use crate::helper;
use crate::shared::{self, Live};
use crate::tray::Mood;
use eframe::egui::{self, Color32, RichText, ViewportCommand};
use lodo_core::peers::{self, Peer};
use lodo_core::{domain, probe, Config, Route, Server, ServerStatus, VpnTarget};
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    advanced: bool,
    error: Option<String>,
    peers: Option<Vec<Peer>>,
    peers_task: Option<Task<Vec<Peer>>>,
    test: Option<Vec<(String, String, bool)>>,
    test_task: Option<Task<Vec<(String, String, bool)>>>,
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
    confirm_delete: Option<usize>,
    autostart: bool,
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
            confirm_delete: None,
            autostart: shared::autolaunch().and_then(|a| a.is_enabled().ok()).unwrap_or(false),
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
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                self.header(ui);
                ui.add_space(6.0);
                if let Some((err, msg)) = self.message.clone() {
                    let color = if err { Color32::from_rgb(220, 60, 50) } else { Color32::from_rgb(40, 160, 80) };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(msg).color(color));
                        if ui.small_button("Tutup").clicked() {
                            self.message = None;
                        }
                    });
                    ui.add_space(6.0);
                }
                let helper_version = self.live.lock().unwrap().helper_version.clone();
                match helper_version {
                    None => self.setup_screen(ui),
                    Some(v) => {
                        if v != APP_VERSION && self.install_task.is_none() {
                            card(ui, |ui| {
                                ui.label("Helper lodo perlu diperbarui ke versi baru.");
                                if ui.button("Perbarui (perlu izin admin)").clicked() {
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
        ui.horizontal(|ui| {
            ui.heading(RichText::new("lodo").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let [r, g, b] = mood.rgb();
                ui.label(RichText::new(text).color(Color32::from_rgb(r, g, b)).strong());
                dot(ui, Color32::from_rgb(r, g, b));
            });
        });
        ui.separator();
    }

    fn setup_screen(&mut self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.label(RichText::new("Selamat datang!").size(20.0).strong());
            ui.add_space(4.0);
            ui.label(
                "lodo membuat satu alamat tetap untuk server rumahmu, misalnya git.home.arpa.\n\n\
                 • Di rumah: otomatis pakai IP rumah.\n\
                 • Di luar: otomatis lewat VPN (Netbird, Tailscale, WireGuard, ZeroTier).\n\n\
                 Kamu tidak perlu ganti setting lagi tiap pindah tempat.",
            );
            ui.add_space(8.0);
            ui.label(RichText::new("Untuk mengatur alamat, lodo perlu izin admin satu kali saja.").weak());
            ui.add_space(8.0);
            if self.install_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Menunggu izin admin…");
                });
            } else if ui.button(RichText::new("Mulai").size(17.0)).clicked() {
                self.message = None;
                self.install_task = Some(spawn_task(ui.ctx(), helper::install));
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
                ui.label(RichText::new("Belum ada server").strong());
                ui.label("Tambahkan server rumahmu (misalnya git, NAS, atau home server).");
            });
        }

        let mut delete: Option<usize> = None;
        for (i, s) in config.servers.iter().enumerate() {
            let fqdn = s.fqdn(&config.domain);
            let st = status.iter().find(|x| x.fqdn == fqdn);
            card(ui, |ui| {
                // Teks di kiri (boleh terlipat), tombol di kanan.
                egui::containers::Sides::new().shrink_left().show(
                    ui,
                    |ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&fqdn).strong().size(16.0));
                            let (color, text) = route_text(st);
                            ui.horizontal_wrapped(|ui| {
                                dot(ui, color);
                                ui.label(RichText::new(text).color(color));
                            });
                        });
                    },
                    |ui| {
                        // Ditambahkan dari kanan ke kiri.
                        if self.confirm_delete == Some(i) {
                            if ui.button(RichText::new("Ya, hapus").color(Color32::from_rgb(220, 60, 50))).clicked() {
                                delete = Some(i);
                            }
                            if ui.button("Batal").clicked() {
                                self.confirm_delete = None;
                            }
                        } else {
                            if ui.button("Hapus").clicked() {
                                self.confirm_delete = Some(i);
                            }
                            if ui.button("Ubah").clicked() {
                                next = Screen::Edit(Box::new(Editor::new(&ctx, Some(i), Some(s))));
                            }
                            if s.web_port.is_some_and(|p| p != 0)
                                && ui.button("Buka").on_hover_text(format!("Buka http://{fqdn} di browser")).clicked()
                            {
                                ctx.open_url(egui::OpenUrl::new_tab(format!("http://{fqdn}")));
                            }
                            if ui.button("Salin").on_hover_text("Salin alamat").clicked() {
                                ctx.copy_text(fqdn.clone());
                                self.message = Some((false, format!("{fqdn} disalin.")));
                            }
                        }
                    },
                );
            });
        }
        if let Some(i) = delete {
            let mut c = config.clone();
            c.servers.remove(i);
            match self.save_config(c) {
                Ok(()) => self.message = Some((false, "Server dihapus.".into())),
                Err(e) => self.message = Some((true, e)),
            }
            self.confirm_delete = None;
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("+ Tambah server").strong()).clicked() {
                next = Screen::Edit(Box::new(Editor::new(&ctx, None, None)));
            }
            if ui.button("Cek ulang").clicked() {
                thread::spawn(helper::recheck);
                self.message = Some((false, "Mengecek ulang…".into()));
            }
            if ui.button("Pengaturan").clicked() {
                next = Screen::Settings(Settings { domain: config.domain.clone(), confirm_uninstall: false });
            }
        });

        if let Some(first) = config.servers.first() {
            ui.add_space(14.0);
            ui.label(RichText::new("Cara pakai").strong());
            ui.label(
                RichText::new(format!(
                    "Pakai alamat di atas di git, ssh, atau browser, sama di mana pun kamu berada. Contoh:\n\
                     git clone ssh://git@{}/user/repo.git\n\
                     ssh user@{}",
                    first.fqdn(&config.domain),
                    first.fqdn(&config.domain)
                ))
                .weak(),
            );
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

        ui.label(RichText::new(if ed.index.is_some() { "Ubah server" } else { "Tambah server" }).size(19.0).strong());
        ui.add_space(6.0);

        egui::Grid::new("form").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label("Nama");
            ui.vertical(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut ed.name).hint_text("mis. git").desired_width(220.0));
                if r.changed() {
                    ed.name = sanitize_label(&ed.name);
                }
                if !ed.name.is_empty() {
                    ui.label(RichText::new(format!("Alamat: {}.{}", ed.name, config.domain)).weak());
                }
            });
            ui.end_row();

            ui.label("IP di rumah");
            ui.add(egui::TextEdit::singleline(&mut ed.home_ip).hint_text("mis. 192.168.1.10").desired_width(220.0));
            ui.end_row();

            ui.label("IP lewat VPN");
            ui.vertical(|ui| {
                let mut remove = None;
                for (i, v) in ed.vpn.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut v.label).hint_text("Nama VPN").desired_width(90.0));
                        ui.add(egui::TextEdit::singleline(&mut v.ip).hint_text("IP VPN").desired_width(130.0));
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
                            ui.label(RichText::new("Mencari Netbird/Tailscale…").weak());
                        }
                        Some(peers) if peers.is_empty() => {
                            ui.label(RichText::new("Netbird/Tailscale tidak terdeteksi, isi manual.").weak());
                        }
                        Some(peers) => {
                            let mut picked = None;
                            egui::ComboBox::from_id_salt("peers")
                                .selected_text("Pilih dari VPN…")
                                .width(170.0)
                                .show_ui(ui, |ui| {
                                    for p in peers {
                                        if ui.selectable_label(false, format!("{} · {}  ({})", p.vpn, p.name, p.ip)).clicked() {
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
            });
            ui.end_row();

            ui.label("Port web");
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut ed.web_port).hint_text("mis. 3002 (boleh kosong)").desired_width(170.0));
                let host = if ed.name.trim().is_empty() { "nama".to_string() } else { ed.name.trim().to_string() };
                let hint = match ed.web_port.trim().parse::<u16>() {
                    Ok(p) if p != 80 && p != 0 => format!("Cukup buka http://{host}.{} tanpa \":{p}\".", config.domain),
                    _ => "Isi kalau aplikasi webnya jalan di port selain 80, supaya tidak perlu mengetik :port.".into(),
                };
                ui.label(RichText::new(hint).weak());
            });
            ui.end_row();
        });

        ui.add_space(4.0);
        egui::CollapsingHeader::new("Lanjutan").default_open(ed.advanced).show(ui, |ui| {
            ed.advanced = true;
            egui::Grid::new("adv").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Port untuk tes");
                ui.vertical(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut ed.port).desired_width(70.0));
                    ui.label(RichText::new("22 = SSH/git. Pakai 80 atau 443 untuk server web.").weak());
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
                        let fwd: Vec<String> = ed.to_server().forwards().iter().skip(1).map(|(p, _)| p.to_string()).collect();
                        ui.label(
                            RichText::new(format!(
                                "Karena port web diisi, hanya port tertentu yang diteruskan. Sekarang: 80, {}. Tambahkan port lain yang kamu pakai.",
                                fwd.join(", ")
                            ))
                            .weak(),
                        );
                    });
                    ui.end_row();
                }
            });
        });

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let testing = ed.test_task.is_some();
            if ui.add_enabled(!testing, egui::Button::new("Tes koneksi")).clicked() {
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
                ui.label(RichText::new("Isi minimal satu IP yang valid dulu.").weak());
            }
            for (label, ip, ok) in results {
                let (c, mark) = if *ok { (Color32::from_rgb(40, 160, 80), "Bisa") } else { (Color32::from_rgb(220, 60, 50), "Tidak bisa") };
                ui.horizontal(|ui| {
                    dot(ui, c);
                    ui.label(RichText::new(format!("{mark} dijangkau lewat {label} ({ip})")).color(c));
                });
            }
        }

        if let Some(e) = &ed.error {
            ui.add_space(4.0);
            ui.label(RichText::new(e).color(Color32::from_rgb(220, 60, 50)));
        }

        ui.add_space(10.0);
        let mut next = None;
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Simpan").strong()).clicked() {
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
            if ui.button("Batal").clicked() {
                next = Some(Screen::Main);
            }
        });
        next.unwrap_or(Screen::Edit(ed))
    }

    fn settings_screen(&mut self, ui: &mut egui::Ui, mut st: Settings) -> Screen {
        let ctx = ui.ctx().clone();
        let Some(config) = self.config.clone() else { return Screen::Main };
        let mut next = None;

        ui.label(RichText::new("Pengaturan").size(19.0).strong());
        ui.add_space(6.0);

        card(ui, |ui| {
            ui.label(RichText::new("Akhiran alamat").strong());
            ui.horizontal(|ui| {
                ui.label("server.");
                ui.add(egui::TextEdit::singleline(&mut st.domain).desired_width(200.0));
            });
            let d = st.domain.trim().trim_end_matches('.').to_ascii_lowercase();
            let check = domain::validate_suffix(&d);
            match &check {
                Ok(()) => ui.label(RichText::new(format!("Aman, tidak bentrok dengan internet. Contoh: git.{d}")).color(Color32::from_rgb(40, 160, 80))),
                Err(e) => ui.label(RichText::new(e.as_str()).color(Color32::from_rgb(220, 60, 50))),
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
            let mut on = self.autostart;
            if ui.checkbox(&mut on, "Buka lodo otomatis saat komputer menyala").changed() {
                self.set_autostart(on);
            }
            ui.label(RichText::new("Pergantian alamat tetap berjalan walau aplikasi ini ditutup.").weak());
        });

        card(ui, |ui| {
            ui.label(RichText::new("Hapus lodo").strong());
            ui.label(RichText::new("Menghapus helper dan semua alamat lodo dari sistem.").weak());
            if self.uninstall_task.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Menunggu izin admin…");
                });
            } else if st.confirm_uninstall {
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Ya, hapus").color(Color32::from_rgb(220, 60, 50))).clicked() {
                        self.uninstall_task = Some(spawn_task(&ctx, helper::uninstall));
                        st.confirm_uninstall = false;
                    }
                    if ui.button("Batal").clicked() {
                        st.confirm_uninstall = false;
                    }
                });
            } else if ui.button("Hapus dari sistem…").clicked() {
                st.confirm_uninstall = true;
            }
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Kembali").clicked() {
                next = Some(Screen::Main);
            }
            ui.label(RichText::new(format!("lodo {APP_VERSION}")).weak());
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
            extra_ports: s.as_ref().map(|s| s.extra_ports.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default(),
            advanced: false,
            error: None,
            peers: None,
            peers_task,
            test: None,
            test_task: None,
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

fn route_text(st: Option<&ServerStatus>) -> (Color32, String) {
    let rgb = |m: Mood| {
        let [r, g, b] = m.rgb();
        Color32::from_rgb(r, g, b)
    };
    match st {
        Some(ServerStatus { route: Route::Home, ip: Some(ip), .. }) => (rgb(Mood::Home), format!("{ip} · jaringan rumah")),
        Some(ServerStatus { route: Route::Vpn(v), ip: Some(ip), .. }) => (rgb(Mood::Vpn), format!("{ip} · lewat {v}")),
        Some(ServerStatus { route: Route::Unreachable, .. }) => {
            (rgb(Mood::Down), "Tidak terjangkau. Cek WiFi atau nyalakan VPN.".into())
        }
        _ => (rgb(Mood::Idle), "Sedang mengecek…".into()),
    }
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
    let r = egui::Frame::group(ui.style())
        .corner_radius(10.0)
        .inner_margin(12.0)
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
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
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
