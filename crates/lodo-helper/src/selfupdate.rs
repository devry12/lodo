//! Helper memperbarui dirinya sendiri dari server rilis.
//!
//! Helper jalan dengan hak admin, jadi bisa mengganti binary-nya tanpa
//! meminta password lagi. File baru hanya dipasang kalau manifest-nya
//! bertanda tangan sah dan checksum-nya cocok (lihat lodo_core::update).

use crate::service;
use lodo_core::update;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const FIRST_CHECK: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Ganti binary di `target` dengan versi terbaru. `Ok(None)` = sudah terbaru.
pub fn update_binary(target: &Path) -> Result<Option<String>, String> {
    let Some(m) = update::check(VERSION)? else { return Ok(None) };
    let asset = m.asset("helper").ok_or("The latest release has no service build for this platform yet.")?;
    let tmp = target.with_extension("download");
    update::download(asset, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    // Windows tidak bisa menimpa exe yang sedang jalan, tapi bisa mengganti namanya.
    #[cfg(windows)]
    {
        let old = target.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(target, &old).map_err(|e| format!("Couldn't replace the service: {e}"))?;
    }
    std::fs::rename(&tmp, target).map_err(|e| format!("Couldn't install the new service: {e}"))?;
    Ok(Some(m.version))
}

pub fn update_cli() -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    match update_binary(&me)? {
        Some(v) => println!("lodo-helper updated to {v}: {}", me.display()),
        None => println!("lodo-helper is up to date ({VERSION})."),
    }
    Ok(())
}

fn is_installed_service() -> Option<PathBuf> {
    let me = std::env::current_exe().ok()?.canonicalize().ok()?;
    let installed = service::installed_path().canonicalize().ok()?;
    (me == installed).then_some(me)
}

/// Jalankan pemeriksa pembaruan di belakang. Kirim `()` untuk cek sekarang.
pub fn spawn() -> Sender<()> {
    let (tx, rx) = mpsc::channel::<()>();
    #[cfg(windows)]
    if let Ok(me) = std::env::current_exe() {
        let _ = std::fs::remove_file(me.with_extension("old"));
    }
    thread::spawn(move || {
        let Some(me) = is_installed_service() else { return };
        let mut wait = FIRST_CHECK;
        loop {
            match rx.recv_timeout(wait) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            wait = CHECK_EVERY;
            match update_binary(&me) {
                Ok(Some(v)) => {
                    eprintln!("lodo-helper: updated to {v}, restarting");
                    restart(&me);
                }
                Ok(None) => {}
                Err(e) => eprintln!("lodo-helper: update failed: {e}"),
            }
        }
    });
    tx
}

/// macOS/Linux: launchd (KeepAlive) dan systemd (Restart=always) menjalankan
/// ulang helper setelah keluar. Windows: jalankan binary baru lalu keluar.
fn restart(me: &Path) -> ! {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new(me).arg("run").spawn();
    }
    let _ = me;
    std::process::exit(0)
}
