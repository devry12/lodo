//! Mengelola blok khusus lodo di file hosts. Baris lain tidak pernah disentuh.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BEGIN: &str = "# BEGIN lodo (dikelola otomatis oleh lodo, jangan diedit)";
pub const END: &str = "# END lodo";

/// Lokasi file hosts. `LODO_HOSTS_FILE` bisa dipakai untuk pengujian.
pub fn hosts_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LODO_HOSTS_FILE") {
        return PathBuf::from(p);
    }
    if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        PathBuf::from(root).join(r"System32\drivers\etc\hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

/// Satu baris hosts: IP + nama-nama.
pub type Entry = (String, Vec<String>);

/// Buang blok lodo lama (kalau ada) dari isi hosts.
pub fn strip_block(content: &str) -> String {
    let nl = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = Vec::new();
    let mut inside = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("# BEGIN lodo") {
            inside = true;
            continue;
        }
        if inside && t == END {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    // Rapikan baris kosong di ujung yang kita tinggalkan.
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut s = out.join(nl);
    if !s.is_empty() {
        s.push_str(nl);
    }
    s
}

/// Ganti blok lodo dengan entri baru. Entri kosong = blok dihapus.
pub fn apply_block(content: &str, entries: &[Entry]) -> String {
    let nl = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let mut s = strip_block(content);
    if entries.is_empty() {
        return s;
    }
    if !s.is_empty() {
        s.push_str(nl);
    }
    s.push_str(BEGIN);
    s.push_str(nl);
    for (ip, names) in entries {
        s.push_str(&format!("{ip}\t{}{nl}", names.join(" ")));
    }
    s.push_str(END);
    s.push_str(nl);
    s
}

/// Tulis hosts dengan aman. Mengembalikan true kalau isinya berubah.
pub fn update_file(path: &Path, entries: &[Entry]) -> io::Result<bool> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let new = apply_block(&old, entries);
    if new == old {
        return Ok(false);
    }
    let backup = path.with_extension("lodo-backup");
    if !backup.exists() && !old.is_empty() {
        let _ = std::fs::write(&backup, &old);
    }
    write_atomic(path, &new)?;
    Ok(true)
}

#[cfg(unix)]
fn write_atomic(path: &Path, data: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let tmp = path.with_extension("lodo-tmp");
    std::fs::write(&tmp, data)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
    std::fs::rename(&tmp, path)
}

#[cfg(windows)]
fn write_atomic(path: &Path, data: &str) -> io::Result<()> {
    // Antivirus kadang mengunci hosts sebentar; coba beberapa kali.
    let mut last = None;
    for _ in 0..5 {
        match std::fs::write(path, data) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(300));
            }
        }
    }
    Err(last.unwrap())
}

/// Bersihkan cache DNS OS supaya perubahan langsung terasa.
pub fn flush_dns() {
    let run = |cmd: &str, args: &[&str]| {
        let _ = Command::new(cmd).args(args).output();
    };
    if cfg!(target_os = "macos") {
        run("/usr/bin/dscacheutil", &["-flushcache"]);
        run("/usr/bin/killall", &["-HUP", "mDNSResponder"]);
    } else if cfg!(windows) {
        run("ipconfig", &["/flushdns"]);
    } else {
        run("resolvectl", &["flush-caches"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "127.0.0.1\tlocalhost\n::1\tlocalhost\n";

    fn e(ip: &str, n: &str) -> Entry {
        (ip.into(), vec![n.into()])
    }

    #[test]
    fn adds_and_replaces_block() {
        let a = apply_block(BASE, &[e("192.168.1.10", "git.home.arpa")]);
        assert!(a.starts_with(BASE));
        assert!(a.contains("192.168.1.10\tgit.home.arpa"));
        let b = apply_block(&a, &[e("100.80.1.1", "git.home.arpa")]);
        assert!(!b.contains("192.168.1.10"));
        assert_eq!(b.matches(BEGIN).count(), 1);
        // idempoten
        assert_eq!(apply_block(&b, &[e("100.80.1.1", "git.home.arpa")]), b);
    }

    #[test]
    fn remove_restores_original() {
        let a = apply_block(BASE, &[e("10.0.0.1", "x.lan")]);
        assert_eq!(apply_block(&a, &[]), BASE);
    }

    #[test]
    fn keeps_lines_after_block_and_crlf() {
        let src = "a\r\n# BEGIN lodo old\r\n1.1.1.1 y.lan\r\n# END lodo\r\nb\r\n";
        let out = apply_block(src, &[e("10.0.0.1", "x.lan")]);
        assert!(out.contains("a\r\nb\r\n"));
        assert!(out.contains("10.0.0.1\tx.lan\r\n"));
        assert!(!out.contains("1.1.1.1"));
    }

    #[test]
    fn update_file_reports_change() {
        let dir = std::env::temp_dir().join(format!("lodo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hosts");
        std::fs::write(&p, BASE).unwrap();
        assert!(update_file(&p, &[e("10.0.0.1", "x.lan")]).unwrap());
        assert!(!update_file(&p, &[e("10.0.0.1", "x.lan")]).unwrap());
        assert!(update_file(&p, &[]).unwrap());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BASE);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
