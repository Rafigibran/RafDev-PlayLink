# RafDev PlayLink

Mirror. Control. Stream.

Aplikasi desktop native untuk mirroring layar Android, kontrol device, dan output live-streaming. Dibangun sesuai PRD v1.0.

- Platform: **Windows 10/11 64-bit** dan **macOS 12+ (Apple Silicon)**. Build macOS belum dites on-device.
- Bahasa: **Rust**, UI: **Slint**
- Prinsip: Minimal UI. Native Performance. Seamless Streaming.

## Screenshots

Tampilan pertama, belum ada HP terhubung:

![Tampilan pertama PlayLink](docs/screenshots/Tampilan%20pertama%20aplikasi%20PlayLink.webp)

Halaman Mirror saat mirroring aktif:

![Halaman Mirror saat mirroring](docs/screenshots/Halaman%20Mirror%20saat%20mirroring.webp)

Halaman Devices:

![Halaman Devices](docs/screenshots/Halaman%20Devices.webp)

Halaman Streaming:

![Halaman Streaming](docs/screenshots/Halaman%20Streaming.webp)

Halaman Recordings:

![Halaman Recordings](docs/screenshots/Halaman%20Recordings.webp)

Halaman Settings:

![Halaman Settings](docs/screenshots/Halaman%20Settings.webp)

Catatan: screenshot Mirror, Devices, Streaming, Recordings, dan Settings memakai data contoh (HP Xiaomi 13T dan layar HP sintetis), karena build ini belum dites on-device.

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

Belum ada verifikasi on-device dari build ini. Build pertama harus dites di PC Windows atau Mac dengan HP Android sungguhan.

## Cara build (GitHub Actions, cara utama)

Repo ini memakai GitHub Actions, jadi tidak butuh PC untuk build.

### Windows

1. Push kode ke branch `main` di repo `Rafigibran/RafDev-PlayLink`.
2. Buka tab **Actions** di GitHub, pilih run **Windows Build** yang terbaru.
3. Tunggu sampai semua step hijau: Check, Test, Build.
4. Unduh artifact **`playlink-windows-x86_64`** dari halaman run tersebut.
5. Ekstrak zip, jalankan `playlink.exe`.

Isi artifact: `playlink.exe`, `README.md`, `THIRD-PARTY-NOTICES.md`, `LICENSE`.

### macOS (Apple Silicon)

1. Push kode ke branch `main` di repo `Rafigibran/RafDev-PlayLink`.
2. Buka tab **Actions** di GitHub, pilih run **macOS Build** yang terbaru.
3. Tunggu sampai semua step hijau: Check, Test, Build.
4. Unduh artifact **`playlink-macos-arm64`** dari halaman run tersebut.
5. Ekstrak zip, seret `PlayLink.app` ke folder Applications, lalu jalankan.

Isi artifact: `PlayLink.app`, `README.md`, `THIRD-PARTY-NOTICES.md`, `LICENSE`.

Catatan Gatekeeper: build ini tidak ditandatangani dengan sertifikat Apple Developer, jadi macOS akan memblokir saat pertama dibuka ("cannot be opened because the developer cannot be verified"). Klik kanan `PlayLink.app`, pilih **Open**, lalu klik **Open** di dialog. Setelah itu aplikasi jalan normal. Build Mac Intel tidak tersedia dari CI; pengguna Intel bisa build dari source dengan target `x86_64-apple-darwin`.

## Build lokal

Butuh Rust stable 1.81 atau lebih baru.

Windows (PowerShell):

```powershell
cargo test --release
cargo build --release
```

Hasilnya di `target\release\playlink.exe`.

macOS (Terminal):

```bash
cargo test --release
cargo build --release
```

Hasilnya di `target/release/playlink`. Untuk membungkus jadi `PlayLink.app`, salin `macos/Info.plist` ke `PlayLink.app/Contents/Info.plist` dan binary ke `PlayLink.app/Contents/MacOS/playlink`.

## First run: unduhan otomatis

Saat pertama kali mirroring, PlayLink mengunduh dua komponen (tidak dibundel di repo):

1. **scrcpy-server** v3.3.3 dari GitHub release Genymobile/scrcpy (Apache-2.0). Bila unduhan versi pin gagal, fallback ke release terbaru lewat GitHub API.
2. **Android platform-tools** (ADB) bila `adb` tidak ditemukan di PATH dan tidak diatur di Settings.

Keduanya disimpan di folder data lokal aplikasi: `%LOCALAPPDATA%\RafDev\PlayLink` di Windows, `~/Library/Application Support/RafDev/PlayLink` di macOS, `~/.local/share/RafDev/PlayLink` di Linux. Bila `adb` tidak ditemukan, PlayLink mengunduh platform-tools dari Google sekali saat pertama refresh device (sekitar 15 MB); bila unduhan gagal, pasang ADB manual atau atur path-nya di Settings.

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
