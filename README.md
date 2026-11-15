# lodo

Satu alamat tetap untuk server rumah, di rumah maupun di luar.

Kamu cukup pakai `git.home.arpa` (atau nama lain). lodo yang memilih jalurnya:

- **Di rumah**: otomatis pakai IP rumah, mis. `192.168.1.10`
- **Di luar**: otomatis lewat VPN: Netbird, Tailscale, WireGuard, ZeroTier, dll.

Tidak perlu ganti remote git, config ssh, atau bookmark lagi tiap pindah tempat.

## Cara pakai (untuk semua orang)

1. Install: buka `.dmg` (Mac), `-setup.exe` (Windows), atau `.deb` / `.AppImage` (Linux).
2. Buka **lodo**, klik **Mulai**, lalu masukkan password admin. Ini cuma sekali.
3. Klik **+ Tambah server**:
   - **Nama**: mis. `git`, jadi alamatnya `git.home.arpa`
   - **IP di rumah**: mis. `192.168.1.10`
   - **IP lewat VPN**: pilih dari daftar Netbird/Tailscale (terdeteksi otomatis), atau klik **+ Isi manual**
   - **Port web** (boleh kosong): kalau aplikasi webnya jalan di port lain, mis. Gitea di `3002`, isi `3002`. Setelah itu cukup buka `http://git.home.arpa` tanpa `:3002`, atau klik **Buka**.
   - Klik **Tes koneksi** untuk melihat jalur mana yang bisa dipakai sekarang.
4. Selesai. Pakai alamatnya di mana saja:
   ```bash
   git remote set-url origin ssh://git@git.home.arpa/user/repo.git
   ```

Ikon di menu bar / tray menunjukkan status:
hijau = di rumah, biru = lewat VPN, merah = ada server yang tidak terjangkau.

## Akhiran domain

Defaultnya `home.arpa`, yang dicadangkan resmi (RFC 8375) khusus untuk jaringan rumah, jadi tidak akan bentrok dengan situs internet mana pun. Akhiran bisa diganti di **Pengaturan**. Yang diterima:

| Diterima | Ditolak |
|---|---|
| `home.arpa`, `internal`, `lan`, `home`, `test`, `intranet`, `private`, `corp`, dan turunannya (mis. `rumah.internal`) | `.local` (dipakai Bonjour/printer), dan semua domain publik (`.com`, `.id`, `.dev`, …, dicek ke Public Suffix List) |

## Cara kerjanya

```
lodo (tray, ~14 MB)  ──►  lodo-helper (service admin, ~1 MB)  ──►  file hosts
       │                        │
       └─ jendela pengaturan    └─ tiap 30 detik + saat jaringan berubah:
          (proses terpisah,        coba IP rumah dulu, lalu IP VPN berurutan
           hanya saat dibuka)      (koneksi TCP ke port 22), yang pertama tembus dipakai
```

- **lodo-helper** satu-satunya bagian yang butuh hak admin. Ia menulis blok khusus di file hosts:
  ```
  # BEGIN lodo (dikelola otomatis oleh lodo, jangan diedit)
  192.168.1.10	git.home.arpa
  # END lodo
  ```
  Baris lain di file hosts tidak pernah disentuh. Backup asli disimpan sebagai `hosts.lodo-backup`.
- **Port web**: file hosts tidak bisa memetakan port. Jadi server yang punya port web diberi alamat loopback sendiri (`127.77.0.2`, `127.77.0.3`, …). Nama lodo mengarah ke alamat itu, dan helper meneruskan koneksi TCP ke IP yang sedang dipilih: port 80 ke port web, plus port web itu sendiri, port tes (mis. 22), dan **Port lain** di bagian Lanjutan. Port yang tidak terdaftar tidak diteruskan. Di macOS helper menambah alias `lo0` untuk alamat itu.
- Service: launchd (macOS), systemd (Linux), Task Scheduler sebagai SYSTEM (Windows).
- App dan helper bicara lewat `127.0.0.1:47811`. Helper memvalidasi setiap config, jadi nama yang bisa diatur hanya nama di bawah akhiran domain pribadi.
- **Hapus lodo** (di Pengaturan) melepas service dan membersihkan blok lodo dari file hosts.

## Struktur project

| Crate | Isi |
|---|---|
| `crates/lodo-core` | validasi domain, editor file hosts, tes koneksi, deteksi peer Netbird/Tailscale, protokol IPC |
| `crates/lodo-helper` | service berhak admin (`run`, `install`, `uninstall`) |
| `crates/lodo-app` | tray (winit + tray-icon) dan jendela (egui), pure Rust tanpa webview |

## Pengembangan

```bash
cargo test --workspace
```

Jalankan helper tanpa root, memakai file sementara:

```bash
LODO_HOSTS_FILE=/tmp/hosts LODO_CONFIG=/tmp/lodo.json LODO_LOOPBACK=127.0.0.1 LODO_IPC_ADDR=127.0.0.1:47911 \
  cargo run -p lodo-helper -- run
```

`LODO_LOOPBACK` mengganti alamat loopback pertama, dan `LODO_IPC_ADDR` memakai port IPC lain supaya tidak bentrok dengan helper yang sudah terpasang. Port di bawah 1024 (mis. 80) tetap butuh root.

```bash
cargo run -p lodo-app --bin lodo
```

Buat installer (perlu `cargo install cargo-packager --locked`):

```bash
cd crates/lodo-app && cargo packager --release
```

Hasilnya ada di `dist/`. GitHub Actions (`.github/workflows/release.yml`) membuat installer untuk macOS, Windows, dan Linux.

Bantuan debug: `LODO_DEBUG_SCREEN=edit|settings` membuka jendela langsung di layar tertentu, `LODO_SCREENSHOT=/path/prefix` menyimpan screenshot jendela tiap 3 detik (format PPM).

Dependensi build Linux: `libxkbcommon-dev libgl1-mesa-dev libdbus-1-dev`.
