//! HTTPS untuk alamat lodo: CA lokal per komputer + sertifikat server.
//!
//! - CA dibuat sekali, disimpan di folder data helper (kunci hanya bisa
//!   dibaca admin/root), dan didaftarkan ke trust store sistem.
//! - CA diberi *name constraints*: hanya boleh menandatangani nama di bawah
//!   akhiran pribadi (home.arpa, internal, lan, ...). Kalaupun kuncinya bocor,
//!   tidak bisa dipakai memalsukan situs internet.
//! - Sertifikat server dibuat di memori untuk semua nama yang memakai HTTPS,
//!   berlaku 90 hari, dan diperbarui otomatis.

use lodo_core::domain::SAFE_SUFFIXES;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, GeneralSubtree, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, NameConstraints,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};
use time::{Duration, OffsetDateTime};

pub const CA_NAME: &str = "lodo Local CA";
const LEAF_DAYS: i64 = 90;
/// Buat ulang sertifikat server setelah umur ini (jauh sebelum kedaluwarsa).
const LEAF_RENEW: StdDuration = StdDuration::from_secs(30 * 24 * 3600);

/// Folder data helper (sama dengan folder config).
pub fn data_dir() -> PathBuf {
    crate::config_path().parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
}

fn ca_file() -> PathBuf {
    data_dir().join("ca.pem")
}

/// Lokasi sertifikat CA (publik, boleh dibaca semua pengguna), kalau sudah dibuat.
pub fn ca_path() -> Option<String> {
    let p = ca_file();
    p.exists().then(|| p.display().to_string())
}

fn ca_key_file() -> PathBuf {
    data_dir().join("ca-key.pem")
}

fn trusted_marker() -> PathBuf {
    data_dir().join("ca-trusted")
}

pub struct Ca {
    pub cert_pem: String,
    key: KeyPair,
}

impl Ca {
    /// Muat CA, atau buat baru kalau belum ada. Aman dipanggil bersamaan
    /// oleh dua proses: yang kalah memakai CA milik yang menang.
    pub fn load_or_create() -> Result<Self, String> {
        if let Ok(ca) = Self::load() {
            return Ok(ca);
        }
        std::fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, CA_NAME);
        dn.push(DnType::OrganizationName, "lodo");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        params.name_constraints = Some(NameConstraints {
            permitted_subtrees: SAFE_SUFFIXES.iter().map(|s| GeneralSubtree::DnsName((*s).into())).collect(),
            excluded_subtrees: vec![],
        });
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(3650);
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;

        // Tulis ke file sementara lalu hard link: gagal kalau file sudah ada,
        // jadi tidak pernah menimpa CA yang sudah dipercaya.
        let pid = std::process::id();
        let tmp_key = data_dir().join(format!(".ca-key.{pid}"));
        let tmp_cert = data_dir().join(format!(".ca.{pid}"));
        write_private(&tmp_key, &key.serialize_pem())?;
        std::fs::write(&tmp_cert, cert.pem()).map_err(|e| e.to_string())?;
        let won = std::fs::hard_link(&tmp_key, ca_key_file()).is_ok();
        if won {
            let _ = std::fs::remove_file(ca_file());
            std::fs::hard_link(&tmp_cert, ca_file()).map_err(|e| e.to_string())?;
        }
        let _ = std::fs::remove_file(&tmp_key);
        let _ = std::fs::remove_file(&tmp_cert);
        if !won {
            // Proses lain lebih dulu; tunggu sebentar sampai sertifikatnya tertulis.
            std::thread::sleep(StdDuration::from_millis(300));
        }
        Self::load()
    }

    fn load() -> Result<Self, String> {
        let key_pem = std::fs::read_to_string(ca_key_file()).map_err(|e| e.to_string())?;
        let cert_pem = std::fs::read_to_string(ca_file()).map_err(|e| e.to_string())?;
        let key = KeyPair::from_pem(&key_pem).map_err(|e| e.to_string())?;
        Ok(Self { cert_pem, key })
    }

    /// SHA-256 sidik jari CA (hex), untuk mencatat CA mana yang sudah dipercaya.
    pub fn fingerprint(&self) -> String {
        let der = pem_to_der(&self.cert_pem).unwrap_or_default();
        lodo_core::update::sha256_hex(&der)
    }

    /// Sertifikat server untuk `names`, ditandatangani CA ini.
    pub fn leaf(&self, names: &[String]) -> Result<Arc<rustls::ServerConfig>, String> {
        let issuer = Issuer::from_ca_cert_pem(&self.cert_pem, &self.key).map_err(|e| e.to_string())?;
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let mut params = CertificateParams::new(names.to_vec()).map_err(|e| e.to_string())?;
        params.distinguished_name.push(DnType::CommonName, names.first().cloned().unwrap_or_default());
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(LEAF_DAYS);
        let cert = params.signed_by(&key, &issuer).map_err(|e| e.to_string())?;

        let chain = vec![cert.der().clone(), rustls::pki_types::CertificateDer::from(pem_to_der(&self.cert_pem)?)];
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into());
        let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(chain, key_der)
            .map_err(|e| e.to_string())?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(cfg))
    }
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body.trim()).map_err(|e| e.to_string())
}

fn write_private(path: &Path, content: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        f.write_all(content.as_bytes()).map_err(|e| e.to_string())
    }
    #[cfg(not(unix))]
    {
        // ProgramData\lodo hanya bisa ditulis admin; kunci ikut terlindungi ACL folder.
        std::fs::write(path, content).map_err(|e| e.to_string())
    }
}

/// Apakah CA yang sekarang sudah didaftarkan ke trust store?
pub fn is_trusted() -> bool {
    let Ok(ca) = Ca::load() else { return false };
    std::fs::read_to_string(trusted_marker()).map(|m| m.trim() == ca.fingerprint()).unwrap_or(false)
}

/// Daftarkan CA ke trust store sistem (butuh admin; di macOS memunculkan dialog izin).
pub fn trust() -> Result<(), String> {
    let ca = Ca::load_or_create()?;
    install_trust(&ca_file())?;
    std::fs::write(trusted_marker(), ca.fingerprint()).map_err(|e| e.to_string())?;
    Ok(())
}

/// Lepas CA dari trust store dan hapus file-nya (dipakai saat uninstall).
pub fn untrust_and_delete() {
    remove_trust();
    for f in [ca_file(), ca_key_file(), trusted_marker()] {
        let _ = std::fs::remove_file(f);
    }
}

fn sh(cmd: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("{cmd}: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

#[cfg(target_os = "macos")]
const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";

#[cfg(target_os = "macos")]
fn install_trust(pem: &Path) -> Result<(), String> {
    remove_trust();
    sh(
        "/usr/bin/security",
        &["add-trusted-cert", "-d", "-r", "trustRoot", "-p", "ssl", "-k", SYSTEM_KEYCHAIN, &pem.display().to_string()],
    )
    .map_err(|e| format!("Gagal mendaftarkan sertifikat lodo: {e}"))
}

#[cfg(target_os = "macos")]
fn remove_trust() {
    // Hapus semua CA lodo lama (bisa lebih dari satu kalau pernah dibuat ulang).
    for _ in 0..10 {
        if sh("/usr/bin/security", &["delete-certificate", "-c", CA_NAME, "-t", SYSTEM_KEYCHAIN]).is_err() {
            break;
        }
    }
}

#[cfg(windows)]
fn install_trust(pem: &Path) -> Result<(), String> {
    remove_trust();
    sh("certutil.exe", &["-f", "-addstore", "Root", &pem.display().to_string()])
        .map_err(|e| format!("Gagal mendaftarkan sertifikat lodo: {e}"))
}

#[cfg(windows)]
fn remove_trust() {
    for _ in 0..10 {
        if sh("certutil.exe", &["-delstore", "Root", CA_NAME]).is_err() {
            break;
        }
    }
}

/// (folder anchor, perintah pembaruan) untuk distro umum.
#[cfg(all(unix, not(target_os = "macos")))]
const LINUX_STORES: &[(&str, &str, &[&str])] = &[
    ("/usr/local/share/ca-certificates", "update-ca-certificates", &[]), // Debian, Ubuntu
    ("/etc/pki/ca-trust/source/anchors", "update-ca-trust", &["extract"]), // Fedora, RHEL
    ("/etc/ca-certificates/trust-source/anchors", "trust", &["extract-compat"]), // Arch
];

#[cfg(all(unix, not(target_os = "macos")))]
fn install_trust(pem: &Path) -> Result<(), String> {
    for (dir, cmd, args) in LINUX_STORES {
        if Path::new(dir).is_dir() {
            std::fs::copy(pem, Path::new(dir).join("lodo-local-ca.crt")).map_err(|e| e.to_string())?;
            return sh(cmd, args).map_err(|e| format!("Gagal mendaftarkan sertifikat lodo: {e}"));
        }
    }
    Err("Trust store sistem tidak dikenali.".into())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn remove_trust() {
    for (dir, cmd, args) in LINUX_STORES {
        let f = Path::new(dir).join("lodo-local-ca.crt");
        if f.exists() {
            let _ = std::fs::remove_file(f);
            let _ = sh(cmd, args);
        }
    }
}

/// Sertifikat server yang sedang dipakai, diperbarui saat nama berubah atau sudah tua.
#[derive(Default)]
pub struct LeafCache {
    names: Vec<String>,
    made: Option<Instant>,
    config: Option<Arc<rustls::ServerConfig>>,
}

impl LeafCache {
    pub fn get(&mut self, names: &[String]) -> Option<Arc<rustls::ServerConfig>> {
        if names.is_empty() {
            self.config = None;
            self.names.clear();
            return None;
        }
        let stale = self.made.is_none_or(|t| t.elapsed() > LEAF_RENEW);
        if self.config.is_none() || stale || self.names != names {
            // Windows/Linux: helper bisa mendaftarkan CA sendiri tanpa dialog.
            // macOS mewajibkan persetujuan pengguna, jadi app yang melakukannya.
            if !cfg!(target_os = "macos")
                && !is_trusted()
                && let Err(e) = trust()
            {
                eprintln!("lodo-helper: {e}");
            }
            match Ca::load_or_create().and_then(|ca| ca.leaf(names)) {
                Ok(cfg) => {
                    self.config = Some(cfg);
                    self.names = names.to_vec();
                    self.made = Some(Instant::now());
                }
                Err(e) => eprintln!("lodo-helper: gagal membuat sertifikat HTTPS: {e}"),
            }
        }
        self.config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_verifies_against_ca_and_respects_constraints() {
        let dir = std::env::temp_dir().join(format!("lodo-ca-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: test tunggal yang memakai variabel ini.
        unsafe { std::env::set_var("LODO_CONFIG", dir.join("config.json")) };
        let ca = Ca::load_or_create().unwrap();
        let again = Ca::load_or_create().unwrap();
        assert_eq!(ca.fingerprint(), again.fingerprint(), "CA tidak boleh dibuat ulang");
        assert!(!is_trusted());

        // HTTPS lewat forwarder sampai ke "server" HTTP biasa.
        let backend = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let backend_addr = backend.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for mut c in backend.incoming().flatten() {
                let mut buf = [0u8; 1024];
                let n = c.read(&mut buf).unwrap();
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /") { "halo gitea" } else { "?" };
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let listen = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut fwd = crate::forward::Forwarder::default();
        let names = vec!["git.home.arpa".to_string(), "gitea.home.arpa".to_string()];
        let target = crate::forward::Target { to: Some(backend_addr), tls: true };
        fwd.apply(&std::collections::HashMap::from([(listen, target)]), Some(ca.leaf(&names).unwrap()));

        let ok = https_get(&ca, listen, "gitea.home.arpa").unwrap();
        assert!(ok.ends_with("halo gitea"), "{ok}");
        // Nama yang tidak ada di sertifikat ditolak.
        assert!(https_get(&ca, listen, "nas.home.arpa").is_err());

        // Name constraints: sertifikat untuk domain publik dari CA ini ditolak klien.
        fwd.apply(&std::collections::HashMap::from([(listen, target)]), Some(ca.leaf(&["google.com".into()]).unwrap()));
        let err = https_get(&ca, listen, "google.com").unwrap_err();
        assert!(err.to_lowercase().contains("name"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn https_get(ca: &Ca, addr: std::net::SocketAddr, host: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        let mut roots = rustls::RootCertStore::empty();
        roots.add(rustls::pki_types::CertificateDer::from(pem_to_der(&ca.cert_pem)?)).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(cfg), name).unwrap();
        let mut sock = std::net::TcpStream::connect(addr).unwrap();
        let mut tls = rustls::Stream::new(&mut conn, &mut sock);
        write!(tls, "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
        let mut out = String::new();
        match tls.read_to_string(&mut out) {
            Ok(_) => Ok(out),
            Err(e) if !out.is_empty() && e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(out),
            Err(e) => Err(e.to_string()),
        }
    }
}
