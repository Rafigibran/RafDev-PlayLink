# RafDev PlayLink

Mirror. Control. Stream.

Aplikasi desktop native untuk mirroring layar Android, kontrol device, dan output live-streaming. Dibangun sesuai PRD v1.0.

- Platform utama: **Windows 10/11 64-bit**
- Bahasa: **Rust**, UI: **Slint**
- Prinsip: Minimal UI. Native Performance. Seamless Streaming.

## Status Fase 1 (MVP)

Fitur yang sudah jadi di kode ini:

| Fitur | Status |
| --- | --- |
| Deteksi device lewat ADB (USB dan Wi-Fi) | Jadi |
| Pairing Wi-Fi (QR belum, pakai kode pairing ADB) | Jadi |
| Mirroring layar (server scrcpy, decode H.264) | Jadi |
| Kontrol: tap, swipe, scroll, tombol Back/Home/Recent, volume | Jadi |
| Fullscreen, pengaturan kualitas video | Jadi |
| Clean Feed: jendela borderless berisi hanya layar HP, siap Window Capture di OBS | Jadi |
| Screenshot (PNG) dan recording (MP4, fallback raw H.264) | Jadi |
| Settings (tersimpan TOML) dan diagnostics export | Jadi |
| Audio forwarding | **Belum**: plumbing dan status sudah ada, decoder nyata dipasang di Fase 2 |
| Virtual camera Windows | **Belum**: status selalu Not Supported di Fase 1, pakai Clean Feed untuk OBS |

Belum ada verifikasi on-device dari build ini. Build pertama harus dites di PC Windows dengan HP Android sungguhan.

## Cara build (GitHub Actions, cara utama)

Repo ini memakai GitHub Actions, jadi tidak butuh PC untuk build.

1. Push kode ke branch `main` di repo `Rafigibran/RafDev-PlayLink`.
2. Buka tab **Actions** di GitHub, pilih run **Windows Build** yang terbaru.
3. Tunggu sampai semua step hijau: Check, Test, Build.
4. Unduh artifact **`playlink-windows-x86_64`** dari halaman run tersebut.
5. Ekstrak zip, jalankan `playlink.exe`.

Isi artifact: `playlink.exe`, `README.md`, `THIRD-PARTY-NOTICES.md`, `LICENSE`.

## Build lokal (Windows)

Butuh Rust stable 1.81 atau lebih baru.

```powershell
cargo test --release
cargo build --release
```

Hasilnya di `target\release\playlink.exe`.

## First run: unduhan otomatis

Saat pertama kali mirroring, PlayLink mengunduh dua komponen (tidak dibundel di repo):

1. **scrcpy-server** v3.3.3 dari GitHub release Genymobile/scrcpy (Apache-2.0). Bila unduhan versi pin gagal, fallback ke release terbaru lewat GitHub API.
2. **Android platform-tools** (ADB) bila `adb` tidak ditemukan di PATH dan tidak diatur di Settings.

Keduanya disimpan di folder data lokal aplikasi (`%LOCALAPPDATA%\RafDev\PlayLink` di Windows).

## Cara pakai

1. Aktifkan **USB debugging** di HP (Settings, Developer options).
2. Colok HP lewat USB, atau hubungkan Wi-Fi: jalankan `adb tcpip 5555` sekali lewat USB, lalu `adb connect <IP HP>:5555`.
3. Buka PlayLink, halaman **Devices**, klik **Refresh**, pilih HP, klik **Connect**.
4. Klik **Start Mirroring**. Layar HP muncul di preview.
5. Klik atau drag di preview untuk tap dan swipe di HP. Tombol Back, Home, dan Recent ada di bawah preview.
6. Untuk OBS: buka halaman **Streaming**, klik **Open Clean Feed**, lalu di OBS tambah **Window Capture** dan pilih jendela "PlayLink Clean Feed".

## Shortcut

| Shortcut | Aksi |
| --- | --- |
| Esc | Back |
| Ctrl+H | Home |
| Ctrl+R | Recent Apps |
| Ctrl+Shift+F | Fullscreen |
| Ctrl+Shift+S | Screenshot |
| Ctrl+Shift+R | Record |

## Pengaturan video

| Preset | Resolusi dan frame rate |
| --- | --- |
| Low | 720p30 |
| Balanced (default) | 1080p30 |
| High | 1080p60 |
| Custom | 24 s/d 60 FPS, diatur di Settings |

Screenshot tersimpan sebagai `PlayLink_Screenshot_YYYY-MM-DD_HH-MM-SS.png`.
Recording tersimpan sebagai `PlayLink_Recording_YYYY-MM-DD_HH-MM-SS.mp4` (H.264/AAC).

## Diagnostics

Halaman **Settings** punya tombol **Export Diagnostics**. Isinya log aplikasi, versi, dan status komponen. Kirim file ini bila melaporkan bug.

## Struktur repo

```
Cargo.toml
build.rs
assets/
ui/            app.slint, clean_feed.slint
src/
  main.rs      entry point
  app.rs       wiring UI ke engine
  core/        error, types, config (Settings TOML), state
  device/      ADB, DeviceManager, saved devices
  mirror/      scrcpy server, decode H.264, frame pipeline
  input/       protokol kontrol scrcpy (tap, swipe, key)
  audio/       plumbing audio (decoder di Fase 2)
  streaming/   Clean Feed, virtual camera
  recording/   screenshot, muxer MP4
  diagnostics/ logging dan export
  platform/    kode khusus Windows
tests/         config, device, recording
.github/workflows/windows-build.yml
```

## Lisensi

Kode PlayLink sendiri berlisensi **MIT** (lihat `LICENSE`, Copyright 2026 RafDev).

Komponen pihak ketiga: lihat `THIRD-PARTY-NOTICES.md`. Catatan penting: Slint tersedia di bawah GPL-3.0, lisensi royalty-free, atau lisensi komersial. Pilihan lisensi Slint untuk distribusi PlayLink perlu dikonfirmasi sebelum rilis publik.
