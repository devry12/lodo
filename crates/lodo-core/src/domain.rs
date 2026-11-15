//! Validasi akhiran domain supaya tidak bentrok dengan domain publik.

/// Akhiran yang aman dipakai di jaringan pribadi.
/// - home.arpa: RFC 8375 (khusus jaringan rumah)
/// - internal: dicadangkan ICANN (2024) untuk private use
/// - test, localhost: RFC 6761
/// - lan, home, intranet, private, corp: tidak akan pernah didelegasikan ICANN
pub const SAFE_SUFFIXES: &[&str] = &[
    "home.arpa", "internal", "test", "localhost", "lan", "home", "intranet", "private", "corp",
];

/// Cek akhiran domain. Mengembalikan pesan ramah bila ditolak.
pub fn validate_suffix(domain: &str) -> Result<(), String> {
    let d = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() {
        return Err("Akhiran domain tidak boleh kosong.".into());
    }
    for label in d.split('.') {
        validate_label(label)?;
    }
    if d == "local" || d.ends_with(".local") {
        return Err("\".local\" dipakai perangkat Apple/printer (Bonjour). Pilih yang lain, mis. home.arpa.".into());
    }
    if SAFE_SUFFIXES.iter().any(|s| d == *s || d.ends_with(&format!(".{s}"))) {
        return Ok(());
    }
    let tld = d.rsplit('.').next().unwrap_or(&d);
    let is_public = psl::suffix(tld.as_bytes()).map(|s| s.is_known()).unwrap_or(false);
    if is_public {
        return Err(format!(
            "\".{tld}\" adalah domain internet sungguhan, bisa bentrok dengan situs asli. \
             Pakai home.arpa, internal, atau lan."
        ));
    }
    Err(format!(
        "\".{tld}\" belum dijamin aman. Pakai salah satu: {}.",
        SAFE_SUFFIXES[..4].join(", ")
    ))
}

/// Satu bagian nama (mis. "git"): huruf kecil, angka, tanda minus.
pub fn validate_label(label: &str) -> Result<(), String> {
    let l = label.trim();
    if l.is_empty() {
        return Err("Nama tidak boleh kosong.".into());
    }
    if l.len() > 63 {
        return Err(format!("Nama \"{l}\" terlalu panjang (maks 63 karakter)."));
    }
    let ok_chars = l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok_chars || l.starts_with('-') || l.ends_with('-') {
        return Err(format!(
            "Nama \"{l}\" hanya boleh huruf kecil, angka, dan tanda minus (tidak di awal/akhir)."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_suffixes_ok() {
        for d in ["home.arpa", "internal", "rumah.internal", "rumah.home.arpa", "lan", "test"] {
            assert!(validate_suffix(d).is_ok(), "{d}");
        }
    }

    #[test]
    fn rejects_local_and_public() {
        for d in ["local", "x.local", "example.com", "dev", "kantor.id", "foo.app"] {
            assert!(validate_suffix(d).is_err(), "{d}");
        }
    }

    #[test]
    fn rejects_bad_labels() {
        for d in ["", "Home.Arpa ", "a..b", "-x.lan", "x_y.lan"] {
            // "Home.Arpa " -> lowercased & trimmed is fine
            if d == "Home.Arpa " {
                assert!(validate_suffix(d).is_ok());
            } else {
                assert!(validate_suffix(d).is_err(), "{d}");
            }
        }
        assert!(validate_label("git").is_ok());
        assert!(validate_label("Git").is_err());
    }
}
