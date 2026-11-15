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
    // LODO_WINDOW_LOCK: jalankan jendela kedua untuk debug tanpa mengganggu yang terbuka.
    let lock = std::env::var("LODO_WINDOW_LOCK").unwrap_or_else(|_| shared::WINDOW_LOCK.into());
    let Ok(listener) = std::net::TcpListener::bind(&lock) else {
        shared::poke(&lock);
        return;
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("lodo")
            .with_inner_size(debug_size().unwrap_or([560.0, 680.0]))
            .with_min_inner_size([460.0, 420.0]),
        ..Default::default()
    };
    let _ = eframe::run_native("lodo", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, listener)))));
}

/// Debug: `LODO_DEBUG_SIZE=560x1500` membuka jendela dengan ukuran tertentu (untuk screenshot).
fn debug_size() -> Option<[f32; 2]> {
    let v = std::env::var("LODO_DEBUG_SIZE").ok()?;
    let (w, h) = v.split_once('x')?;
    Some([w.parse().ok()?, h.parse().ok()?])
}
