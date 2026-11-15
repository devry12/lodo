//! Pembaruan otomatis.
//!
//! Server rilis (Cloudflare Worker + R2, lihat `worker/`) menyajikan:
//!   /latest.json       manifest versi terbaru
//!   /latest.json.sig   tanda tangan ed25519 atas isi latest.json (base64)
//!   /releases/<versi>/<file>
//!
//! Manifest hanya dipercaya kalau tanda tangannya cocok dengan `PUBLIC_KEY`
//! yang tertanam di aplikasi, dan setiap file dicek SHA-256-nya sebelum dipakai.
//! Jadi walau server diambil alih, file palsu tidak akan dipasang.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

pub const DEFAULT_URL: &str = "https://lodo.kreasimaju.id";

/// Kunci publik penanda tangan rilis (pasangan dari kunci rahasia di CI).
pub const PUBLIC_KEY: &str = "q8k63zVNVqfnZ3UIVeIOUUXM3nJUrhVjeBiXdqPwaPg=";

/// Batas ukuran file unduhan, sekadar pengaman.
const MAX_DOWNLOAD: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub date: String,
    /// Kunci: "<platform>-<jenis>", mis. "darwin-aarch64-app", "linux-x86_64-helper".
    pub assets: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Relatif terhadap alamat server rilis, mis. "releases/0.3.0/lodo-helper-linux-x86_64".
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    pub fn asset(&self, kind: &str) -> Option<&Asset> {
        self.assets.get(&format!("{}-{kind}", platform()))
    }
}

/// Alamat server rilis. `LODO_UPDATE_URL` untuk pengujian.
pub fn base_url() -> String {
    std::env::var("LODO_UPDATE_URL").unwrap_or_else(|_| DEFAULT_URL.into()).trim_end_matches('/').to_string()
}

/// Nama platform yang sedang berjalan, mis. "darwin-aarch64".
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// Apakah versi `remote` lebih baru dari `current` ("0.10.0" > "0.9.3").
pub fn is_newer(remote: &str, current: &str) -> bool {
    fn parse(v: &str) -> Vec<u64> {
        v.trim().trim_start_matches('v').split(['.', '-', '+']).take(3).map(|p| p.parse().unwrap_or(0)).collect()
    }
    parse(remote) > parse(current)
}

/// Cek tanda tangan lalu baca manifest.
pub fn verify_manifest(json: &[u8], sig_b64: &str, public_key_b64: &str) -> Result<Manifest, String> {
    let key: [u8; 32] = B64
        .decode(public_key_b64.trim())
        .ok()
        .and_then(|k| k.try_into().ok())
        .ok_or("The update public key is invalid.")?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| "The update public key is invalid.")?;
    let sig: [u8; 64] = B64
        .decode(sig_b64.trim())
        .ok()
        .and_then(|s| s.try_into().ok())
        .ok_or("The update signature is corrupted.")?;
    key.verify_strict(json, &Signature::from_bytes(&sig))
        .map_err(|_| "The update signature doesn't match; the update was ignored.")?;
    serde_json::from_slice(json).map_err(|e| format!("Unrecognized update manifest: {e}"))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .user_agent(concat!("lodo-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get_bytes(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let resp = agent().get(url).call().map_err(|e| format!("Couldn't reach the update server: {e}"))?;
    resp.into_body().with_config().limit(limit).read_to_vec().map_err(|e| format!("Download failed: {e}"))
}

/// Ambil manifest terbaru yang tanda tangannya sah.
pub fn fetch_manifest() -> Result<Manifest, String> {
    let base = base_url();
    let json = get_bytes(&format!("{base}/latest.json"), 1024 * 1024)?;
    let sig = get_bytes(&format!("{base}/latest.json.sig"), 4096)?;
    verify_manifest(&json, &String::from_utf8_lossy(&sig), PUBLIC_KEY)
}

/// Kembalikan manifest kalau ada versi yang lebih baru dari `current`.
pub fn check(current: &str) -> Result<Option<Manifest>, String> {
    let m = fetch_manifest()?;
    Ok(is_newer(&m.version, current).then_some(m))
}

/// Unduh `asset` ke `dest` dan pastikan SHA-256-nya cocok.
pub fn download(asset: &Asset, dest: &Path) -> Result<(), String> {
    if asset.size > MAX_DOWNLOAD {
        return Err("The update file is too large.".into());
    }
    let url = if asset.url.starts_with("https://") { asset.url.clone() } else { format!("{}/{}", base_url(), asset.url) };
    let resp = agent().get(&url).call().map_err(|e| format!("Couldn't download the update: {e}"))?;
    let mut reader = resp.into_body().into_with_config().limit(MAX_DOWNLOAD).reader();
    let tmp = dest.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("Couldn't create the file: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| format!("Couldn't write the file: {e}"))?;
    }
    drop(file);
    let got = hex(&hasher.finalize());
    if !got.eq_ignore_ascii_case(asset.sha256.trim()) {
        let _ = std::fs::remove_file(&tmp);
        return Err("The update file is corrupted (checksum mismatch).".into());
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("Couldn't save the update: {e}"))
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(hex(&h.finalize()));
        }
        h.update(&buf[..n]);
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn signature() {
        use ed25519_dalek::{Signer, SigningKey};
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = B64.encode(sk.verifying_key().to_bytes());
        let json = br#"{"version":"9.9.9","assets":{}}"#;
        let sig = B64.encode(sk.sign(json).to_bytes());
        assert_eq!(verify_manifest(json, &sig, &pk).unwrap().version, "9.9.9");
        // Isi diubah sedikit saja -> ditolak.
        let tampered = br#"{"version":"9.9.8","assets":{}}"#;
        assert!(verify_manifest(tampered, &sig, &pk).is_err());
        // Kunci lain -> ditolak.
        let other = B64.encode(SigningKey::from_bytes(&[8u8; 32]).verifying_key().to_bytes());
        assert!(verify_manifest(json, &sig, &other).is_err());
    }
}
