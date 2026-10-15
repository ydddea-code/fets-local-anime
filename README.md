# Fets Anime

Fets Anime adalah aplikasi desktop Windows 10/11 x64 untuk mengelola dan menonton anime dari file video di komputer sendiri.

**Versi:** 0.1.0

## Unduh dan pasang

Unduh installer Windows dari [GitHub Releases](https://github.com/ydddea-code/fets-local-anime/releases/latest), lalu buka file `.exe`. Installer memasang aplikasi untuk akun Windows Anda dan membuat shortcut di Start Menu serta Desktop. Jika diperlukan, installer juga menyiapkan WebView2.

Setelah terpasang, buka **Fets Anime** dari Start Menu atau Desktop.

## Mulai menonton

1. Buka **Pengaturan → Tambah Folder** dan pilih folder koleksi anime.
2. Tunggu pemindaian selesai; file video akan muncul di perpustakaan.
3. Pilih anime dan episode untuk mulai menonton.

File baru di folder library akan terdeteksi otomatis. Untuk memulai ulang pemindaian, pilih **Pindai ulang** di Pengaturan.

## Pemutar dan data

Pemutar otomatis mencoba video bawaan aplikasi, lalu beralih ke mpv bila pemutaran gagal. mpv, FFmpeg, dan ffprobe disertakan dalam installer. Jalurnya dapat diganti di Pengaturan bila diperlukan.

Database, pengaturan, thumbnail, dan cache disimpan di `%APPDATA%\FetsAnime`. Saat menghapus aplikasi, installer menanyakan apakah data tersebut ikut dihapus.

Fets Anime hanya memutar file milik pengguna; aplikasi tidak mengambil atau mengunduh video dari situs lain. Penggunaan AniList untuk metadata bersifat opsional.

## Membuat installer dari source

Di Windows 10/11 x64, pasang Rust stable, Visual Studio Build Tools (C++), dan 7-Zip. Dari folder repo jalankan:

```powershell
.\scripts\fetch-sidecars.ps1
cargo install tauri-cli --version "^2"
cd src-tauri
cargo tauri build --bundles nsis
```

Installer dibuat di `src-tauri\target\release\bundle\nsis`. Build menyertakan FFmpeg dan mpv GPL; periksa `licenses/README.md` dan notice yang diunduh sebelum mendistribusikan ulang.

## Pemeriksaan source

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo fmt --manifest-path src-tauri/Cargo.toml --all
cargo check --manifest-path src-tauri/Cargo.toml
```
