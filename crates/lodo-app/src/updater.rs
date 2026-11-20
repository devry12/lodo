//! Pembaruan otomatis aplikasi lodo (tray + jendela).
//!
//! Proses tray memeriksa server rilis saat mulai dan tiap 6 jam. Kalau ada
//! versi baru yang bertanda tangan sah, file diunduh, dicek, dipasang,
//! lalu lodo dibuka ulang. Helper memperbarui dirinya sendiri secara terpisah.

use lodo_core::serde_json::{self, json};
use lodo_core::update::{self, Manifest};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------- preferensi

fn prefs_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(std::env::var_os("HOME")?).join("Library/Application Support")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".config")))?
    };
    Some(base.join("lodo").join("app.json"))
}

/// Pembaruan otomatis aktif? Default: ya.
pub fn auto_update() -> bool {
    prefs_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get("auto_update")?.as_bool())
        .unwrap_or(true)
}

pub fn set_auto_update(on: bool) -> Result<(), String> {
    let p = prefs_path().ok_or("Settings folder not found.")?;
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(p, json!({ "auto_update": on }).to_string()).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- periksa & pasang

pub fn check() -> Result<Option<Manifest>, String> {
    update::check(VERSION)
}

/// Bundle lodo.app yang sedang jalan (macOS).
fn mac_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.ancestors().nth(3)?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

/// Bisa memasang sendiri di instalasi ini? Kalau tidak, pengguna diarahkan ke situs.
pub fn can_self_install() -> bool {
    if cfg!(target_os = "macos") {
        mac_bundle().is_some()
    } else if cfg!(windows) {
        true
    } else {
        std::env::var_os("APPIMAGE").is_some()
    }
}

pub fn download_page() -> String {
    update::base_url()
}

/// Unduh, cek, dan pasang versi `m`, lalu siapkan lodo dibuka ulang setelah
/// proses ini keluar. Pemanggil harus segera keluar kalau berhasil.
pub fn install(m: &Manifest, open_window: bool) -> Result<(), String> {
    let asset = m.asset("app").ok_or("No new version for this system yet.")?;
    let dir = std::env::temp_dir().join(format!("lodo-update-{}", m.version));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(asset.url.rsplit('/').next().unwrap_or("lodo-update"));
    update::download(asset, &file)?;
    install_platform(&file, &dir, open_window)
}

#[cfg(target_os = "macos")]
fn install_platform(archive: &Path, dir: &Path, open_window: bool) -> Result<(), String> {
    let bundle = mac_bundle().ok_or("lodo isn't running from lodo.app.")?;
    run("/usr/bin/tar", &["-xzf", &archive.display().to_string(), "-C", &dir.display().to_string()])?;
    let new_app = dir.join("lodo.app");
    if !new_app.join("Contents/MacOS/lodo").exists() {
        return Err("The update is incomplete.".into());
    }
    // Salin di sebelah bundle lama dulu, lalu tukar dengan rename (cepat dan aman).
    let staged = bundle.with_file_name(".lodo-new.app");
    let old = bundle.with_file_name(".lodo-old.app");
    let _ = std::fs::remove_dir_all(&staged);
    let _ = std::fs::remove_dir_all(&old);
    run("/usr/bin/ditto", &[&new_app.display().to_string(), &staged.display().to_string()])
        .map_err(|_| no_permission(&bundle))?;
    std::fs::rename(&bundle, &old).map_err(|_| no_permission(&bundle))?;
    if let Err(e) = std::fs::rename(&staged, &bundle) {
        let _ = std::fs::rename(&old, &bundle);
        return Err(format!("Couldn't install the new version: {e}"));
    }
    let _ = std::fs::remove_dir_all(&old);
    let _ = std::fs::remove_dir_all(dir);
    // Minta macOS (LaunchServices) membuka versi baru sekarang. Proses baru itu bukan
    // anak proses ini, jadi tidak ikut berhenti saat proses ini keluar; ia sendiri
    // menunggu sampai proses lama selesai (--after-update).
    if std::env::var_os("LODO_NO_RELAUNCH").is_some() {
        return Ok(());
    }
    let mut cmd = Command::new("/usr/bin/open");
    cmd.arg("-n");
    if let Ok(lock) = std::env::var("LODO_TRAY_LOCK") {
        cmd.args(["--env", &format!("LODO_TRAY_LOCK={lock}")]);
    }
    cmd.arg(&bundle).args(["--args", "--after-update"]);
    if !open_window {
        cmd.arg("--hidden");
    }
    let out = cmd.output().map_err(|e| format!("Couldn't reopen lodo: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("Couldn't reopen lodo: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(windows)]
fn install_platform(setup: &Path, _dir: &Path, open_window: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let q = |p: &Path| p.display().to_string().replace('\'', "''");
    let arg = if open_window { "" } else { " -ArgumentList '--hidden'" };
    let script = format!(
        "Wait-Process -Id {} -ErrorAction SilentlyContinue; \
         Start-Process -Wait -FilePath '{}' -ArgumentList '/S'; \
         Start-Process -FilePath '{}'{arg}",
        std::process::id(),
        q(setup),
        q(&exe)
    );
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't run the installer: {e}"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn install_platform(file: &Path, _dir: &Path, open_window: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let target = PathBuf::from(std::env::var_os("APPIMAGE").ok_or("The .deb version is updated from the lodo website.")?);
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let staged = target.with_extension("new");
    std::fs::copy(file, &staged).map_err(|_| no_permission(&target))?;
    std::fs::rename(&staged, &target).map_err(|_| no_permission(&target))?;
    if std::env::var_os("LODO_NO_RELAUNCH").is_some() {
        return Ok(());
    }
    // Proses baru di grup prosesnya sendiri, supaya tidak ikut berhenti bersama proses ini.
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(&target);
    cmd.arg("--after-update").process_group(0);
    if !open_window {
        cmd.arg("--hidden");
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("Couldn't reopen lodo: {e}"))
}

fn no_permission(p: &Path) -> String {
    format!("No permission to replace {}. Download the new version at {}.", p.display(), download_page())
}


#[cfg(target_os = "macos")]
fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}
