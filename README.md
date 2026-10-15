# Anime Stream

Server lokal untuk mengelola dan menonton file anime yang dimiliki pengguna. Versi awal ini menyediakan scan folder, daftar anime dan episode, tambah manual, direct play dengan HTTP Range, serta progres dan lanjutkan menonton. Aplikasi tidak mengunduh atau mengambil video dari situs pihak ketiga.

## Prasyarat

- Rust stable
- FFmpeg dan ffprobe diperlukan untuk info stream, thumbnail, ekstrak subtitle embedded, dan HLS. Direct play serta subtitle eksternal tetap berfungsi tanpa keduanya.

## Menjalankan

Salin `config.example.toml` menjadi `config.toml`, lalu atur folder library:

```toml
[server]
host = "127.0.0.1"
port = 8080

[library]
paths = ["D:/Anime"]
extensions = ["mkv", "mp4", "webm", "avi", "m4v"]
scan_on_startup = false
watch = false

[data]
dir = "./data"

[ffmpeg]
ffmpeg_path = "ffmpeg"
ffprobe_path = "ffprobe"
hls_segment_seconds = 6
max_concurrent_transcodes = 2

[metadata]
anilist_enabled = false
cache_ttl_hours = 24

[auth]
enabled = false
password = ""
```

Jalankan `cargo run`, lalu buka [http://127.0.0.1:8080](http://127.0.0.1:8080). Database disimpan di `data/app.db`. Server default hanya mendengarkan localhost. Override konfigurasi dapat memakai environment variable, misalnya `ANIME_SERVER__PORT=9000`.

Halaman Pengaturan menjalankan scan. Nama file yang cocok akan diparsing; file yang tidak cocok tetap masuk memakai nomor fallback berdasarkan urutan nama file. Path yang dikirim saat menambah file manual harus berada di dalam folder library yang dikonfigurasi.

### Akses LAN

Autentikasi memakai satu password dan cookie sesi HttpOnly yang berlaku tujuh hari (sesi disimpan di memori dan berakhir saat server dimulai ulang). Aktifkan dengan konfigurasi berikut atau environment variable `ANIME_AUTH__ENABLED=true` dan `ANIME_AUTH__PASSWORD=...`:

```toml
[server]
host = "0.0.0.0"

[auth]
enabled = true
password = "ganti-dengan-password-kuat"
```

Jangan mengekspos server ke internet tanpa TLS dan perlindungan jaringan yang sesuai. Tanpa autentikasi, gunakan bind localhost.

### Docker

Build image dengan `docker build -t anime-stream .`. Siapkan `config.toml` di host dengan path `/library`, auth aktif dan password kuat; lalu jalankan:

```powershell
docker run --rm -p 8080:8080 -v "${PWD}/config.toml:/app/config.toml:ro" -v "D:/Anime:/library:ro" -v "anime-data:/data" anime-stream
```

Image runtime menyertakan FFmpeg/ffprobe. Konfigurasi data default diarahkan ke volume `/data`.

## Endpoint

- `GET /` — library dan lanjutkan menonton
- `GET /add` — form tambah anime
- `GET /anime/:id` — episode dan form tambah episode manual
- `GET /watch/:id` — pemutar, progres, dan episode berikutnya
- `GET /stream/:id` — direct play dengan byte ranges
- `GET /hls/:id/master.m3u8` dan `/hls/:id/:segment` — HLS on-demand
- `GET /thumb/:id.jpg` — thumbnail episode
- `GET /subs/:id.vtt` — subtitle WebVTT
- `GET /api/episode/:id/subtitles` — daftar subtitle
- `GET /api/anime`, `POST /api/anime`
- `POST /api/anime/:id/episodes`
- `POST /api/anime/:id/refresh-metadata` — isi metadata dari AniList saat diaktifkan
- `PATCH /api/anime/:id/metadata-lock` — kunci/perbolehkan pembaruan metadata
- `POST /api/library/scan`
- `GET /api/progress/:id`, `POST /api/progress/:id`
- `GET /health`

Klien memakai ID database untuk streaming dan tidak dapat memilih path arbitrer. Server canonicalize dan memeriksa path terhadap folder library sebelum membaca file.

## Status

M0–M9 menyediakan fitur dasar, ffprobe, thumbnail, subtitle eksternal/embedded, HLS on-demand, AniList metadata manual, file watcher opsional, autentikasi password opsional, Dockerfile, dan CI. `hls.js` 1.7.3 disertakan lokal agar pemutaran HLS tidak memerlukan internet. Metadata AniList mati secara default dan dapat diaktifkan melalui `[metadata]`; watcher juga mati secara default dan dapat diaktifkan melalui `[library]`. Pembaruan metadata mempertahankan nama anime yang dimasukkan pengguna dan metadata dapat dikunci dari halaman anime. CI menjalankan fmt, clippy, test, dan build release pada Linux, Windows, serta macOS. Fitur lanjutan yang belum ada mencakup status job scan, Askama/HTMX, dan build installer.

## Pemeriksaan

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```
