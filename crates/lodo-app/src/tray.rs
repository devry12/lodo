//! Ikon di menu bar / system tray.

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    /// Semua server lewat jaringan rumah
    Home,
    /// Minimal satu server lewat VPN
    Vpn,
    /// Ada server yang tidak terjangkau / helper mati
    Down,
    /// Belum ada server / belum dicek
    Idle,
}

impl Mood {
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Mood::Home => [52, 199, 89],
            Mood::Vpn => [10, 132, 255],
            Mood::Down => [255, 69, 58],
            Mood::Idle => [142, 142, 147],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mood::Home => "Di rumah",
            Mood::Vpn => "Lewat VPN",
            Mood::Down => "Ada server tak terjangkau",
            Mood::Idle => "Belum ada server",
        }
    }
}

/// Gambar ikon bulat sederhana (tanpa file gambar).
pub fn icon(mood: Mood) -> Icon {
    const N: u32 = 32;
    let [r, g, b] = mood.rgb();
    let mut px = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // Lingkaran luar berisi warna, dengan tepi halus.
            let a = (11.5 - d).clamp(0.0, 1.0);
            px.extend_from_slice(&[r, g, b, (a * 255.0) as u8]);
        }
    }
    Icon::from_rgba(px, N, N).expect("ikon valid")
}

pub struct Tray {
    pub icon: TrayIcon,
    pub status_item: MenuItem,
    pub open_id: MenuId,
    pub recheck_id: MenuId,
    pub update_item: MenuItem,
    pub quit_id: MenuId,
    mood: Mood,
}

impl Tray {
    pub fn new() -> Option<Self> {
        let status_item = MenuItem::new(Mood::Idle.label(), false, None);
        let open = MenuItem::new("Buka lodo", true, None);
        let recheck = MenuItem::new("Cek ulang sekarang", true, None);
        let update_item = MenuItem::new("Cek pembaruan", true, None);
        let quit = MenuItem::new("Keluar", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &open,
            &recheck,
            &update_item,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .ok()?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("lodo")
            .with_icon(icon(Mood::Idle))
            .build()
            .ok()?;
        Some(Self {
            icon,
            status_item,
            open_id: open.id().clone(),
            recheck_id: recheck.id().clone(),
            update_item,
            quit_id: quit.id().clone(),
            mood: Mood::Idle,
        })
    }

    pub fn set(&mut self, mood: Mood, text: &str) {
        if self.mood != mood {
            let _ = self.icon.set_icon(Some(icon(mood)));
            self.mood = mood;
        }
        self.status_item.set_text(text);
        let _ = self.icon.set_tooltip(Some(format!("lodo — {text}")));
    }
}
