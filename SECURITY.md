# Keamanan

lodo menjalankan helper berhak admin yang menulis file hosts dan memperbarui
dirinya sendiri, jadi laporan keamanan kami tanggapi serius.

## Melaporkan celah

Jangan buka issue publik untuk celah keamanan. Pakai
**Security → Report a vulnerability** (GitHub private vulnerability reporting)
di repo ini. Sertakan langkah untuk mengulang masalahnya dan versi lodo yang dipakai.

## Model keamanan pembaruan

- Rilis resmi hanya diterbitkan dari tag `v*` di repo ini oleh pengelola.
  Kunci penanda tangan tidak pernah ada di repo; CI memakainya dari
  GitHub Environment `release`, yang tidak tersedia untuk pull request atau fork.
- `latest.json` ditandatangani ed25519. Kunci publik tertanam di
  `crates/lodo-core/src/update.rs`; tanda tangan yang tidak cocok ditolak.
- Setiap file dicek SHA-256 sebelum dipasang, dan versi lebih lama tidak pernah dipasang.
- Server rilis (Cloudflare Worker + R2) hanya menyajikan file. Walau server itu
  diambil alih, file yang tidak ditandatangani kunci rahasia tidak akan terpasang.
- Helper hanya mendengarkan di `127.0.0.1` dan memvalidasi setiap config: nama
  yang bisa diatur hanya nama di bawah akhiran domain pribadi (bukan domain publik).
