//! Pasang / hapus helper sebagai service sistem di macOS, Linux, Windows.

use lodo_core::hosts;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ID: &str = "id.lodo.helper";

fn installed_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from(format!("/Library/PrivilegedHelperTools/{ID}"))
    } else if cfg!(windows) {
        let base = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        PathBuf::from(base).join(r"lodo\lodo-helper.exe")
    } else {
        PathBuf::from("/usr/local/lib/lodo/lodo-helper")
    }
}

fn sh(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd} {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

fn copy_self(dest: &Path) -> Result<(), String> {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    if me == dest {
        return Ok(());
    }
    // Salin ke file sementara lalu rename, supaya aman walau versi lama masih jalan.
    let tmp = dest.with_extension("new");
    std::fs::copy(&me, &tmp).map_err(|e| format!("Gagal menyalin helper: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("Gagal memasang helper: {e}"))
}

pub fn install() -> Result<(), String> {
    stop_service();
    let bin = installed_path();
    copy_self(&bin)?;
    start_service(&bin)?;
    println!("lodo-helper terpasang di {}", bin.display());
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    stop_service();
    remove_service_files();
    if hosts::update_file(&hosts::hosts_path(), &[]).unwrap_or(false) {
        hosts::flush_dns();
    }
    crate::forward::remove_loopback_aliases();
    let _ = std::fs::remove_file(installed_path());
    println!("lodo-helper dihapus, file hosts sudah dibersihkan.");
    Ok(())
}

// ---------------- macOS (launchd) ----------------
#[cfg(target_os = "macos")]
fn plist_path() -> String {
    format!("/Library/LaunchDaemons/{ID}.plist")
}

#[cfg(target_os = "macos")]
fn start_service(bin: &Path) -> Result<(), String> {
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{ID}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardErrorPath</key><string>/var/log/lodo-helper.log</string>
</dict>
</plist>
"#,
        bin.display()
    );
    std::fs::write(plist_path(), plist).map_err(|e| format!("Gagal menulis plist: {e}"))?;
    sh("/bin/launchctl", &["bootstrap", "system", &plist_path()])
}

#[cfg(target_os = "macos")]
fn stop_service() {
    let _ = sh("/bin/launchctl", &["bootout", &format!("system/{ID}")]);
}

#[cfg(target_os = "macos")]
fn remove_service_files() {
    let _ = std::fs::remove_file(plist_path());
}

// ---------------- Linux (systemd) ----------------
#[cfg(all(unix, not(target_os = "macos")))]
const UNIT: &str = "/etc/systemd/system/lodo-helper.service";

#[cfg(all(unix, not(target_os = "macos")))]
fn start_service(bin: &Path) -> Result<(), String> {
    let unit = format!(
        "[Unit]\nDescription=lodo helper (alamat server rumah otomatis)\nAfter=network.target\n\n\
         [Service]\nExecStart={} run\nRestart=always\nRestartSec=3\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        bin.display()
    );
    std::fs::write(UNIT, unit).map_err(|e| format!("Gagal menulis unit systemd: {e}"))?;
    sh("systemctl", &["daemon-reload"])?;
    sh("systemctl", &["enable", "--now", "lodo-helper.service"])
}

#[cfg(all(unix, not(target_os = "macos")))]
fn stop_service() {
    let _ = sh("systemctl", &["disable", "--now", "lodo-helper.service"]);
}

#[cfg(all(unix, not(target_os = "macos")))]
fn remove_service_files() {
    let _ = std::fs::remove_file(UNIT);
    let _ = sh("systemctl", &["daemon-reload"]);
}

// ---------------- Windows (Task Scheduler, akun SYSTEM) ----------------
#[cfg(windows)]
fn ps(script: &str) -> Result<(), String> {
    sh("powershell.exe", &["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
}

#[cfg(windows)]
fn start_service(bin: &Path) -> Result<(), String> {
    let exe = bin.display().to_string().replace('\'', "''");
    ps(&format!(
        "$a=New-ScheduledTaskAction -Execute '{exe}' -Argument 'run'; \
         $t=New-ScheduledTaskTrigger -AtStartup; \
         $s=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -RestartCount 999 \
            -RestartInterval (New-TimeSpan -Minutes 1) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; \
         Register-ScheduledTask -TaskName 'lodo-helper' -Action $a -Trigger $t -Settings $s \
            -User 'SYSTEM' -RunLevel Highest -Force | Out-Null; \
         Start-ScheduledTask -TaskName 'lodo-helper'"
    ))
}

#[cfg(windows)]
fn stop_service() {
    let _ = ps("Stop-ScheduledTask -TaskName 'lodo-helper' -ErrorAction SilentlyContinue");
    let _ = sh("taskkill", &["/IM", "lodo-helper.exe", "/F"]);
}

#[cfg(windows)]
fn remove_service_files() {
    let _ = ps("Unregister-ScheduledTask -TaskName 'lodo-helper' -Confirm:$false -ErrorAction SilentlyContinue");
}
