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
        _ => Err("Balasan helper tidak terduga.".into()),
    }
}

pub fn set_config(c: &Config) -> Result<(), String> {
    match ipc::send(&Request::SetConfig(c.clone()))? {
        Response::Ok => Ok(()),
        Response::Error(e) => Err(e),
        _ => Err("Balasan helper tidak terduga.".into()),
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
    if p.exists() { Ok(p) } else { Err(format!("File {name} tidak ditemukan di samping aplikasi.")) }
}

/// Jalankan `lodo-helper <action>` dengan izin admin. OS akan menampilkan dialog password.
fn run_elevated(action: &str) -> Result<(), String> {
    let bin = helper_binary()?;
    let path = bin.display().to_string();
    let out = if cfg!(target_os = "macos") {
        let shell = format!("'{}' {action}", path.replace('\'', "'\\''"));
        let script = format!(
            "do shell script \"{}\" with prompt \"lodo perlu izin admin sekali untuk mengatur alamat server.\" with administrator privileges",
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
            Err("Dibatalkan. Izin admin diperlukan supaya lodo bisa bekerja.".into())
        } else {
            Err(format!("Gagal: {}", err.trim()))
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
    Err("Helper terpasang tapi belum merespons. Coba buka ulang lodo.".into())
}

pub fn uninstall() -> Result<(), String> {
    run_elevated("uninstall")
}
