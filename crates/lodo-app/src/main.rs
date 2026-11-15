//! lodo — satu alamat untuk server rumah, di rumah atau di luar.
//!
//!   lodo            ikon tray + buka jendela
//!   lodo --hidden   ikon tray saja (dipakai saat login)
//!   lodo --window   jendela pengaturan (dijalankan oleh tray)
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod helper;
mod shared;
mod tray;
mod tray_app;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--window") {
        run_window();
    } else {
        tray_app::run(!has("--hidden"));
    }
}

fn run_window() {
    // Hanya satu jendela. Kalau sudah ada, fokuskan yang itu.
    let Ok(listener) = std::net::TcpListener::bind(shared::WINDOW_LOCK) else {
        shared::poke(shared::WINDOW_LOCK);
        return;
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("lodo")
            .with_inner_size([540.0, 520.0])
            .with_min_inner_size([440.0, 380.0]),
        ..Default::default()
    };
    let _ = eframe::run_native("lodo", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, listener)))));
}
