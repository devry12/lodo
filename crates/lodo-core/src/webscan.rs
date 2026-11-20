//! Cari aplikasi web di sebuah server, supaya pengguna tidak perlu tahu nomor port.
//!
//! Mencoba port web yang umum secara paralel. Sebuah port dianggap aplikasi web
//! kalau membalas HTTP (status apa pun). Hanya HTTP biasa yang dicari, karena
//! lodo sendiri yang menyediakan HTTPS di depannya.

use std::io::Read;
use std::net::{IpAddr, SocketAddr};
use std::thread;
use std::time::Duration;

/// Port yang sering dipakai aplikasi rumahan (Gitea, NAS, Home Assistant, Jellyfin, ...).
pub const COMMON_PORTS: &[u16] = &[
    80, 3000, 3001, 3002, 5000, 5001, 8000, 8080, 8081, 8090, 8096, 8123, 8200, 8888, 9000, 9090, 32400,
];

const TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebApp {
    pub port: u16,
    /// Judul halaman, kalau ada (mis. "Gitea").
    pub title: String,
}

/// Aplikasi web yang membalas di `ip`, urut nomor port.
pub fn scan(ip: IpAddr, ports: &[u16]) -> Vec<WebApp> {
    let handles: Vec<_> = ports.iter().map(|&port| thread::spawn(move || check(ip, port))).collect();
    let mut found: Vec<WebApp> = handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect();
    found.sort_by_key(|a| a.port);
    found
}

fn check(ip: IpAddr, port: u16) -> Option<WebApp> {
    let addr = SocketAddr::new(ip, port);
    if !crate::probe::connects(ip, port, Duration::from_millis(1200)) {
        return None;
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .max_redirects(3)
        .build()
        .into();
    let resp = agent.get(format!("http://{addr}/")).call().ok()?;
    let mut body = Vec::new();
    let _ = resp.into_body().into_reader().take(64 * 1024).read_to_end(&mut body);
    Some(WebApp { port, title: title(&String::from_utf8_lossy(&body)) })
}

/// Isi <title>, dirapikan dan dipotong.
fn title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(start) = lower.find("<title") else { return String::new() };
    let Some(open_end) = lower[start..].find('>') else { return String::new() };
    let from = start + open_end + 1;
    let Some(len) = lower[from..].find("</title") else { return String::new() };
    let text: String = html[from..from + len].split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.replace("&amp;", "&").replace("&#39;", "'").replace("&quot;", "\"");
    text.chars().take(40).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn titles() {
        assert_eq!(title("<html><head><TITLE>\n Gitea: Git with a cup of tea \n</TITLE>"), "Gitea: Git with a cup of tea");
        assert_eq!(title("<title data-x=\"1\">NAS &amp; Media</title>"), "NAS & Media");
        assert_eq!(title("no title here"), "");
    }

    #[test]
    fn finds_http_and_skips_others() {
        let serve = |reply: &'static str| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            thread::spawn(move || {
                for mut c in l.incoming().flatten() {
                    let mut buf = [0u8; 512];
                    let _ = c.read(&mut buf);
                    let _ = c.write_all(reply.as_bytes());
                }
            });
            port
        };
        let web = serve("HTTP/1.1 200 OK\r\nContent-Length: 27\r\nConnection: close\r\n\r\n<title>Rumah Server</title>");
        let login = serve("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let not_http = serve("SSH-2.0-OpenSSH_9.2\r\n");
        let closed = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut want = vec![WebApp { port: web, title: "Rumah Server".into() }, WebApp { port: login, title: String::new() }];
        want.sort_by_key(|a| a.port);
        assert_eq!(scan("127.0.0.1".parse().unwrap(), &[web, login, not_http, closed]), want);
    }
}
