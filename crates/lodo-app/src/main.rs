//! lodo — satu alamat untuk server rumah, di rumah atau di luar.
//!
//!   lodo            ikon tray + buka jendela
//!   lodo --hidden   ikon tray saja (dipakai saat login)
//!   lodo --window   jendela pengaturan (dijalankan oleh tray)
//!   lodo --update   periksa dan pasang pembaruan dari terminal
#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod helper;
mod shared;
mod tray;
mod tray_app;
mod updater;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    if has("--window") {
        run_window();
    } else if has("--update") {
        update_cli();
    } else {
        tray_app::run(!has("--hidden"));
    }
}

fn update_cli() {
    match updater::check() {
        Ok(None) => println!("lodo sudah versi terbaru ({}).", updater::VERSION),
        Ok(Some(m)) => {
            println!("Memasang lodo {}…", m.version);
            match updater::install(&m, false) {
                Ok(()) => println!("Selesai. lodo akan dibuka ulang."),
                Err(e) => {
                    eprintln!("Gagal: {e}");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
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
