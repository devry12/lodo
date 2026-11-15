use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub const DEFAULT_DOMAIN: &str = "home.arpa";
pub const DEFAULT_PORT: u16 = 22;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Akhiran domain, mis. "home.arpa" -> git.home.arpa
    pub domain: String,
    pub servers: Vec<Server>,
}

impl Default for Config {
    fn default() -> Self {
        Self { domain: DEFAULT_DOMAIN.into(), servers: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    /// Nama pendek, mis. "git"
    pub name: String,
    /// IP server di jaringan rumah
    pub home_ip: String,
    /// IP alternatif lewat VPN, dicoba berurutan
    #[serde(default)]
    pub vpn: Vec<VpnTarget>,
    /// Port yang dipakai untuk mengetes koneksi
    #[serde(default = "default_port")]
    pub port: u16,
    /// Nama tambahan yang mengarah ke server yang sama
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Port aplikasi web di server (mis. 3002). Kalau diisi, lodo meneruskan
    /// port 80 ke port ini, jadi cukup buka http://git.home.arpa tanpa ":3002".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_port: Option<u16>,
    /// Port lain yang ikut diteruskan apa adanya saat `web_port` aktif.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_ports: Vec<u16>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnTarget {
    /// Label bebas: "Netbird", "Tailscale", "WireGuard", ...
    pub label: String,
    pub ip: String,
}

/// Lewat jalur mana server sedang dijangkau.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Route {
    Home,
    Vpn(String),
    Unreachable,
    /// Belum pernah dicek
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerStatus {
    pub name: String,
    /// Nama lengkap, mis. git.home.arpa
    pub fqdn: String,
    pub ip: Option<String>,
    pub route: Route,
}

impl Server {
    pub fn fqdn(&self, domain: &str) -> String {
        format!("{}.{}", self.name, domain)
    }

    /// Semua nama lengkap (utama + alias).
    pub fn all_names(&self, domain: &str) -> Vec<String> {
        std::iter::once(&self.name)
            .chain(self.aliases.iter())
            .map(|n| format!("{n}.{domain}"))
            .collect()
    }

    /// Port yang diteruskan lodo: (port di alamat lodo, port di server).
    /// Kosong berarti tanpa penerusan: nama langsung mengarah ke IP server.
    pub fn forwards(&self) -> Vec<(u16, u16)> {
        let Some(web) = self.web_port.filter(|&p| p != 80 && p != 0) else {
            return Vec::new();
        };
        let mut out = vec![(80, web)];
        for p in [web, self.port].into_iter().chain(self.extra_ports.iter().copied()) {
            if p != 0 && !out.iter().any(|(l, _)| *l == p) {
                out.push((p, p));
            }
        }
        out
    }

    /// Daftar kandidat berurutan: rumah dulu, lalu VPN.
    pub fn candidates(&self) -> Vec<(Route, IpAddr)> {
        let mut out = Vec::new();
        if let Ok(ip) = self.home_ip.trim().parse() {
            out.push((Route::Home, ip));
        }
        for v in &self.vpn {
            if let Ok(ip) = v.ip.trim().parse() {
                out.push((Route::Vpn(v.label.clone()), ip));
            }
        }
        out
    }
}

impl Config {
    /// Validasi lengkap sebelum disimpan / dipakai helper.
    pub fn validate(&self) -> Result<(), String> {
        crate::domain::validate_suffix(&self.domain)?;
        let mut seen = std::collections::HashSet::new();
        for s in &self.servers {
            crate::domain::validate_label(&s.name)?;
            for a in &s.aliases {
                crate::domain::validate_label(a)?;
            }
            for n in std::iter::once(&s.name).chain(s.aliases.iter()) {
                if !seen.insert(n.to_ascii_lowercase()) {
                    return Err(format!("Nama \"{n}\" dipakai lebih dari sekali."));
                }
            }
            if s.home_ip.trim().parse::<IpAddr>().is_err() {
                return Err(format!("IP rumah untuk \"{}\" tidak valid.", s.name));
            }
            for v in &s.vpn {
                if v.ip.trim().parse::<IpAddr>().is_err() {
                    return Err(format!("IP VPN \"{}\" untuk \"{}\" tidak valid.", v.ip, s.name));
                }
            }
            if s.port == 0 || s.web_port == Some(0) || s.extra_ports.contains(&0) {
                return Err(format!("Port untuk \"{}\" tidak valid.", s.name));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(web: Option<u16>, extra: Vec<u16>) -> Server {
        Server {
            name: "git".into(),
            home_ip: "192.168.1.10".into(),
            vpn: vec![],
            port: 22,
            aliases: vec![],
            web_port: web,
            extra_ports: extra,
        }
    }

    #[test]
    fn forwards() {
        assert!(server(None, vec![2222]).forwards().is_empty());
        assert!(server(Some(80), vec![]).forwards().is_empty());
        assert_eq!(server(Some(3002), vec![]).forwards(), vec![(80, 3002), (3002, 3002), (22, 22)]);
        assert_eq!(server(Some(3002), vec![2222, 22, 80]).forwards(), vec![(80, 3002), (3002, 3002), (22, 22), (2222, 2222)]);
    }

    #[test]
    fn old_config_still_loads() {
        let s: Server = serde_json::from_str(r#"{"name":"git","home_ip":"10.0.0.1"}"#).unwrap();
        assert_eq!(s.web_port, None);
        assert!(s.extra_ports.is_empty());
    }
}
