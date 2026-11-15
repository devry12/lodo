//! Alat rilis lodo (tidak ikut dipasang ke pengguna).
//!
//!   lodo-release keygen <file-kunci>
//!       Buat pasangan kunci. Kunci rahasia disimpan ke <file-kunci>,
//!       kunci publik dicetak untuk ditempel ke lodo-core/src/update.rs.
//!
//!   lodo-release manifest <folder> <versi> [catatan]
//!       Tulis <folder>/latest.json dari file rilis di <folder>, lalu
//!       tanda tangani jadi latest.json.sig. Kunci rahasia dibaca dari
//!       env LODO_SIGNING_KEY (base64) atau file di LODO_SIGNING_KEY_FILE.
//!
//! Nama file rilis yang dikenali (ganti <plat> dengan mis. darwin-aarch64):
//!   lodo-<plat>.app.tar.gz | lodo-<plat>-setup.exe | lodo-<plat>.AppImage   -> <plat>-app
//!   lodo-helper-<plat>[.exe]                                                -> <plat>-helper
//!   lodo-<plat>.dmg -> <plat>-dmg      lodo-<plat>.deb -> <plat>-deb

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use lodo_core::update::{Asset, Manifest, sha256_file};
use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let res = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", path] => keygen(Path::new(path)),
        ["manifest", dir, version] => manifest(Path::new(dir), version, ""),
        ["manifest", dir, version, notes] => manifest(Path::new(dir), version, notes),
        _ => Err("Pemakaian: lodo-release keygen <file> | manifest <folder> <versi> [catatan]".into()),
    };
    if let Err(e) = res {
        eprintln!("lodo-release: {e}");
        std::process::exit(1);
    }
}

fn keygen(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("{} sudah ada, tidak ditimpa.", path.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
    let sk = SigningKey::from_bytes(&seed);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, B64.encode(seed)).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    println!("Kunci rahasia: {}", path.display());
    println!("Kunci publik:  {}", B64.encode(sk.verifying_key().to_bytes()));
    Ok(())
}

fn signing_key() -> Result<SigningKey, String> {
    let b64 = match std::env::var("LODO_SIGNING_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => {
            let f = std::env::var("LODO_SIGNING_KEY_FILE").map_err(|_| "Set LODO_SIGNING_KEY atau LODO_SIGNING_KEY_FILE.")?;
            std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?
        }
    };
    let seed: [u8; 32] =
        B64.decode(b64.trim()).ok().and_then(|k| k.try_into().ok()).ok_or("Kunci rahasia tidak valid.")?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Nama file -> kunci aset, mis. "lodo-helper-linux-x86_64" -> "linux-x86_64-helper".
fn asset_key(file: &str) -> Option<String> {
    let rest = file.strip_prefix("lodo-")?;
    if let Some(plat) = rest.strip_prefix("helper-") {
        return Some(format!("{}-helper", plat.trim_end_matches(".exe")));
    }
    for (suffix, kind) in [(".app.tar.gz", "app"), ("-setup.exe", "app"), (".AppImage", "app"), (".dmg", "dmg"), (".deb", "deb")] {
        if let Some(plat) = rest.strip_suffix(suffix) {
            return Some(format!("{plat}-{kind}"));
        }
    }
    None
}

fn manifest(dir: &Path, version: &str, notes: &str) -> Result<(), String> {
    let version = version.trim_start_matches('v');
    let key = signing_key()?;
    let mut assets = BTreeMap::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(k) = asset_key(&name) else { continue };
        let path = e.path();
        let asset = Asset {
            url: format!("releases/{version}/{name}"),
            sha256: sha256_file(&path).map_err(|e| e.to_string())?,
            size: path.metadata().map_err(|e| e.to_string())?.len(),
        };
        println!("{k:<26} {name}");
        assets.insert(k, asset);
    }
    if assets.is_empty() {
        return Err(format!("Tidak ada file rilis di {}.", dir.display()));
    }
    let date = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let m = Manifest { version: version.into(), notes: notes.into(), date, assets };
    let json = serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?;
    let sig = B64.encode(key.sign(&json).to_bytes());
    std::fs::write(dir.join("latest.json"), &json).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("latest.json.sig"), sig).map_err(|e| e.to_string())?;
    println!("latest.json versi {version} ditandatangani (kunci publik {}).", B64.encode(key.verifying_key().to_bytes()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::asset_key;

    #[test]
    fn keys() {
        assert_eq!(asset_key("lodo-darwin-aarch64.app.tar.gz").unwrap(), "darwin-aarch64-app");
        assert_eq!(asset_key("lodo-windows-x86_64-setup.exe").unwrap(), "windows-x86_64-app");
        assert_eq!(asset_key("lodo-linux-x86_64.AppImage").unwrap(), "linux-x86_64-app");
        assert_eq!(asset_key("lodo-helper-windows-x86_64.exe").unwrap(), "windows-x86_64-helper");
        assert_eq!(asset_key("lodo-helper-darwin-aarch64").unwrap(), "darwin-aarch64-helper");
        assert_eq!(asset_key("lodo-linux-x86_64.deb").unwrap(), "linux-x86_64-deb");
        assert_eq!(asset_key("latest.json"), None);
    }
}
