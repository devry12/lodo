//! Bicara dengan lodo-helper dan memasangnya dengan izin admin (sekali).

use lodo_core::ipc::{self, Request, Response};
use lodo_core::{Config, ServerStatus};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

pub fn ping() -> Option<String> {
    match ipc::send(&Request::Ping) {
        Ok(Response::Pong { version }) => Some(version),
        _ => None,
    }
}

pub fn status() -> Option<Vec<ServerStatus>> {
    match ipc::send(&Request::Status) {
        Ok(Response::Status(s)) => Some(s),
        _ => None,
    }
}

pub fn get_config() -> Result<Config, String> {
    match ipc::send(&Request::GetConfig)? {
        Response::Config(c) => Ok(c),
        Response::Error(e) => Err(e),
        _ => Err("Unexpected reply from the lodo service.".into()),
    }
}

pub fn set_config(c: &Config) -> Result<(), String> {
    match ipc::send(&Request::SetConfig(c.clone()))? {
        Response::Ok => Ok(()),
        Response::Error(e) => Err(e),
        _ => Err("Unexpected reply from the lodo service.".into()),
    }
}

const CA_NAME: &str = "lodo Local CA";

/// Sudah dipercayakah CA lokal untuk HTTPS? None = helper lama / mati / CA belum ada.
pub fn https_trusted() -> Option<bool> {
    let Ok(Response::Https { trusted, ca }) = ipc::send(&Request::HttpsStatus) else { return None };
    if cfg!(target_os = "macos") {
        return ca.map(|p| mac_trusted(&p));
    }
    Some(trusted)
}

/// macOS: cek trust dengan pengaturan pengguna ini. Hasil disimpan sebentar
/// supaya tidak menjalankan `security` tiap 3 detik.
fn mac_trusted(ca: &str) -> bool {
    use std::sync::Mutex;
    static CACHE: Mutex<Option<(String, bool, Instant)>> = Mutex::new(None);
    let fp = std::fs::read(ca).map(|b| lodo_core::update::sha256_hex(&b)).unwrap_or_default();
    let mut c = CACHE.lock().unwrap();
    if let Some((f, ok, at)) = c.as_ref()
        && *f == fp
        && (*ok || at.elapsed() < Duration::from_secs(20))
    {
        return *ok;
    }
    let ok = Command::new("/usr/bin/security")
        .args(["verify-cert", "-q", "-p", "ssl", "-c", ca])
        .output()
        .is_ok_and(|o| o.status.success());
    *c = Some((fp, ok, Instant::now()));
    ok
}

fn login_keychain() -> String {
    format!("{}/Library/Keychains/login.keychain-db", std::env::var("HOME").unwrap_or_default())
}

/// Daftarkan CA lokal lodo supaya HTTPS dipercaya.
/// macOS: ke keychain pengguna (memunculkan dialog password macOS).
/// Windows/Linux: lewat helper dengan izin admin.
pub fn trust_ca() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return run_elevated("trust");
    }
    let ca = match ipc::send(&Request::HttpsStatus)? {
        Response::Https { ca: Some(ca), .. } => ca,
        _ => return Err("lodo's certificate doesn't exist yet. Set a web page on one of your servers first.".into()),
    };
    let kc = login_keychain();
    untrust_ca_user();
    let out = Command::new("/usr/bin/security")
        .args(["add-trusted-cert", "-r", "trustRoot", "-p", "ssl", "-k", &kc, &ca])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("canceled") || err.contains("denied") {
            Err("Cancelled. HTTPS isn't on yet.".into())
        } else {
            Err(format!("Couldn't turn on HTTPS: {}", err.trim()))
        }
    }
}

/// macOS: hapus CA lodo dari keychain pengguna.
pub fn untrust_ca_user() {
    if cfg!(target_os = "macos") {
        let kc = login_keychain();
        for _ in 0..10 {
            let ok = Command::new("/usr/bin/security")
                .args(["delete-certificate", "-c", CA_NAME, "-t", &kc])
                .output()
                .is_ok_and(|o| o.status.success());
            if !ok {
                break;
            }
        }
    }
}

/// Minta helper memperbarui dirinya (tanpa izin admin; helper sudah berhak admin).
pub fn request_update() {
    let _ = ipc::send(&Request::Update);
}

pub fn recheck() {
    let _ = ipc::send(&Request::Recheck);
}

/// lodo-helper berada di folder yang sama dengan aplikasi.
fn helper_binary() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let name = if cfg!(windows) { "lodo-helper.exe" } else { "lodo-helper" };
    let p = me.with_file_name(name);
    if p.exists() { Ok(p) } else { Err(format!("{name} wasn't found next to the app.")) }
}

/// Jalankan `lodo-helper <action>` dengan izin admin. OS akan menampilkan dialog password.
fn run_elevated(action: &str) -> Result<(), String> {
    let bin = helper_binary()?;
    let path = bin.display().to_string();
    let out = if cfg!(target_os = "macos") {
        let shell = format!("'{}' {action}", path.replace('\'', "'\\''"));
        let script = format!(
            "do shell script \"{}\" with prompt \"lodo needs admin permission once to manage server addresses.\" with administrator privileges",
            shell.replace('\\', "\\\\").replace('"', "\\\"")
        );
        Command::new("/usr/bin/osascript").args(["-e", &script]).output()
    } else if cfg!(windows) {
        let script = format!(
            "$p = Start-Process -FilePath '{}' -ArgumentList '{action}' -Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode",
            path.replace('\'', "''")
        );
        Command::new("powershell.exe").args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script]).output()
    } else {
        Command::new("pkexec").args([path.as_str(), action]).output()
    }
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("-128") || err.contains("canceled") || err.contains("dismissed") || out.status.code() == Some(126) {
            Err("Cancelled. lodo needs admin permission to work.".into())
        } else {
            Err(format!("Failed: {}", err.trim()))
        }
    }
}

pub fn install() -> Result<(), String> {
    run_elevated("install")?;
    // Tunggu helper siap.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if ping().is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err("The lodo service was installed but isn't responding. Try reopening lodo.".into())
}

pub fn uninstall() -> Result<(), String> {
    run_elevated("uninstall")?;
    untrust_ca_user();
    Ok(())
}
