//! Deteksi otomatis perangkat (peer) dari Netbird dan Tailscale yang terpasang.

use serde_json::Value;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// "Netbird" atau "Tailscale"
    pub vpn: &'static str,
    /// Nama perangkat
    pub name: String,
    pub ip: String,
}

/// Aplikasi GUI sering tidak mewarisi PATH terminal, jadi cek lokasi umum dulu.
fn run_json(bins: &[&str], args: &[&str]) -> Option<Value> {
    for bin in bins {
        if (bin.contains('/') || bin.contains('\\')) && !Path::new(bin).exists() {
            continue;
        }
        if let Ok(out) = Command::new(bin).args(args).output()
            && out.status.success()
            && let Ok(v) = serde_json::from_slice(&out.stdout)
        {
            return Some(v);
        }
    }
    None
}

const NETBIRD: &[&str] = &[
    "/usr/local/bin/netbird",
    "/opt/homebrew/bin/netbird",
    "/usr/bin/netbird",
    r"C:\Program Files\Netbird\netbird.exe",
    "netbird",
];
const TAILSCALE: &[&str] = &[
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    "/usr/local/bin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/usr/bin/tailscale",
    r"C:\Program Files\Tailscale\tailscale.exe",
    "tailscale",
];

pub fn parse_netbird(v: &Value) -> Vec<Peer> {
    v["peers"]["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let ip = p["netbirdIp"].as_str()?.split('/').next()?.to_string();
            let fqdn = p["fqdn"].as_str().unwrap_or(&ip);
            let name = fqdn.split('.').next().unwrap_or(fqdn).to_string();
            Some(Peer { vpn: "Netbird", name, ip })
        })
        .collect()
}

pub fn parse_tailscale(v: &Value) -> Vec<Peer> {
    v["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|p| {
            let ip = p["TailscaleIPs"].as_array()?.iter().filter_map(|x| x.as_str()).find(|s| s.contains('.'))?;
            let name = p["HostName"].as_str().unwrap_or(ip).to_string();
            Some(Peer { vpn: "Tailscale", name, ip: ip.to_string() })
        })
        .collect()
}

/// Kumpulkan semua peer yang terdeteksi, urut nama.
pub fn detect() -> Vec<Peer> {
    let mut out = Vec::new();
    if let Some(v) = run_json(NETBIRD, &["status", "--json"]) {
        out.extend(parse_netbird(&v));
    }
    if let Some(v) = run_json(TAILSCALE, &["status", "--json"]) {
        out.extend(parse_tailscale(&v));
    }
    out.sort_by(|a, b| a.vpn.cmp(b.vpn).then(a.name.cmp(&b.name)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_netbird() {
        let v: Value = serde_json::from_str(
            r#"{"peers":{"details":[{"fqdn":"mini.netbird.selfhosted","netbirdIp":"100.80.1.2"}]}}"#,
        )
        .unwrap();
        assert_eq!(parse_netbird(&v), vec![Peer { vpn: "Netbird", name: "mini".into(), ip: "100.80.1.2".into() }]);
    }

    #[test]
    fn parses_tailscale() {
        let v: Value = serde_json::from_str(
            r#"{"Peer":{"k":{"HostName":"nas","TailscaleIPs":["fd7a::1","100.64.0.5"]}}}"#,
        )
        .unwrap();
        assert_eq!(parse_tailscale(&v), vec![Peer { vpn: "Tailscale", name: "nas".into(), ip: "100.64.0.5".into() }]);
    }
}
