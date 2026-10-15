# Proyek: Local Anime Streaming Server (Rust)

> Dokumen ini adalah spesifikasi lengkap sekaligus panduan kerja untuk Codex.
> Simpan sebagai `AGENTS.md` di root repo agar Codex membacanya otomatis di setiap sesi.
> Bahasa dokumen: Indonesia. Identifier kode, nama komit, dan komentar kode: Inggris.

---

## 1. Ringkasan

Membuat aplikasi **streaming anime lokal (self-hosted)** seperti Jellyfin/Plex versi ringan, ditulis dengan **Rust**.
Pengguna dapat **menambahkan anime sendiri** (file video milik pribadi) ke dalam library, lalu menontonnya lewat browser di jaringan lokal (LAN) atau di komputer yang sama.

> **Catatan hukum:** Aplikasi ini hanya untuk memutar file yang pengguna miliki secara sah
> (rip pribadi dari DVD/BD, file legal, dsb.). Aplikasi tidak boleh menyertakan fitur
> scraping/download dari situs pihak ketiga.

### Tujuan
- Satu binary yang mudah dijalankan (`cargo run` / satu file executable).
- Tambah anime dengan 2 cara: (a) menaruh file di folder library lalu scan, (b) upload / tambah manual lewat UI.
- Streaming mulus di browser (seek, subtitle, lanjut tonton dari terakhir).
- Metadata otomatis (poster, sinopsis, judul) opsional dari AniList; tetap berfungsi **offline penuh**.

### Non-Tujuan (di luar scope v1)
- Multi-user dengan role kompleks, transcoding hardware-accelerated, aplikasi mobile native, fitur sosial, akses dari internet publik.

---

## 2. Tech Stack

| Kebutuhan | Pilihan |
|---|---|
| Bahasa | Rust (edition 2021, stable terbaru) |
| Async runtime | `tokio` |
| Web framework | `axum` + `tower-http` (static files, CORS, trace, compression) |
| Database | SQLite via `sqlx` (migrations dengan `sqlx migrate`) |
| Template/UI | `askama` (server-rendered) + **HTMX** + CSS sederhana. Alternatif: SPA terpisah (lihat §12) |
| Video playback | HTML5 `<video>` + **hls.js** (untuk HLS) |
| Transcode/remux | **FFmpeg** & **ffprobe** (dipanggil lewat `tokio::process::Command`) |
| Subtitle | SRT/VTT native; ASS/SSA via **JASSUB** (WASM) atau konversi ke VTT |
| Parsing nama file | `regex` + `once_cell`/`LazyLock` |
| Metadata online (opsional) | AniList GraphQL API via `reqwest` |
| Serialisasi | `serde`, `serde_json` |
| Config | `config` / `figment` + file `config.toml` |
| Logging | `tracing`, `tracing-subscriber` |
| Error | `thiserror` (library), `anyhow` (main/glue) |
| File watching | `notify` (auto-scan saat ada file baru) |
| Test | `cargo test`, `axum-test` / `tower::ServiceExt`, `tempfile` |

Prasyarat sistem: Rust toolchain, FFmpeg & ffprobe tersedia di `PATH` (path dapat diatur di config).

---

## 3. Fitur

### 3.1 MVP (v0.1)
1. Konfigurasi folder library (satu atau lebih).
2. Scan folder → deteksi file video (`mkv, mp4, webm, avi, m4v`) → simpan ke DB.
3. Parsing nama file → judul anime, nomor episode, season.
4. Halaman daftar anime (grid poster) dan halaman detail (daftar episode).
5. Pemutar video dengan **HTTP Range** (direct play) sehingga seek berfungsi.
6. Tombol tambah anime manual (nama, folder, poster opsional).
7. Simpan progres tonton per episode, tombol "Lanjutkan".

### 3.2 v0.2
- Ekstraksi info stream dengan `ffprobe` (durasi, codec, audio track, subtitle track).
- Subtitle eksternal (`.srt`, `.vtt`, `.ass`) dan embedded (ekstrak via ffmpeg ke VTT).
- Pilih audio track / subtitle di player.
- Generate thumbnail/poster dari frame video bila tidak ada poster.
- Pencarian dan filter (judul, tag, status ditonton).

### 3.3 v0.3
- **HLS on-demand** (remux/transcode ke segmen `.ts`/`.m4s`) untuk file yang tidak kompatibel browser (mis. HEVC 10-bit, audio FLAC/AC3).
- Metadata AniList (poster, sinopsis, genre, skor) dengan cache lokal.
- File watcher: auto-scan ketika file ditambah/dihapus.
- Auto-play episode berikutnya, tandai "sudah ditonton".

### 3.4 Nanti
- Multi-profil sederhana (PIN), pengaturan tema, PWA, import/export library.
- Catatan: Docker dan build lintas platform (macOS/Linux) tidak lagi didukung; aplikasi khusus Windows.

---

## 4. Arsitektur

```
┌──────────────┐   HTTP    ┌──────────────────────────────────────┐
│   Browser    │◄─────────►│  axum server                         │
│ (HTMX/video) │           │  ├─ routes (pages + JSON API)        │
└──────────────┘           │  ├─ services (library, stream, meta) │
                           │  ├─ db (sqlx / SQLite)               │
                           │  └─ workers (scanner, ffmpeg, watch) │
                           └───────────────┬──────────────────────┘
                                           │ spawn
                                   ┌───────▼───────┐
                                   │ ffmpeg/ffprobe│
                                   └───────────────┘
                           Filesystem: library folders + data dir (db, cache, thumbnails, hls)
```

Prinsip:
- Layering jelas: `routes` → `services` → `db`/`ffmpeg`. Route tidak boleh menulis SQL langsung.
- Semua I/O async; tugas berat (scan, ffmpeg) berjalan di task terpisah dengan progress yang bisa dilihat.
- Tidak ada `unwrap()`/`expect()` di jalur request; gunakan `Result` dengan error type sendiri.
- State bersama lewat `Arc<AppState>` (pool DB, config, handle worker).

---

## 5. Struktur Folder

```
anime-stream/
├─ AGENTS.md                 # file ini
├─ Cargo.toml
├─ config.example.toml
├─ migrations/
│  ├─ 0001_init.sql
│  └─ ...
├─ src/
│  ├─ main.rs                # bootstrap: config, db, router, serve
│  ├─ config.rs
│  ├─ error.rs               # AppError + IntoResponse
│  ├─ state.rs
│  ├─ db/
│  │  ├─ mod.rs
│  │  ├─ anime.rs
│  │  ├─ episode.rs
│  │  └─ progress.rs
│  ├─ services/
│  │  ├─ scanner.rs          # scan folder, upsert ke DB
│  │  ├─ parser.rs           # parse nama file
│  │  ├─ probe.rs            # ffprobe wrapper
│  │  ├─ stream.rs           # range streaming
│  │  ├─ hls.rs              # HLS session manager
│  │  ├─ subtitles.rs
│  │  ├─ metadata.rs         # AniList client + cache
│  │  └─ watcher.rs
│  ├─ routes/
│  │  ├─ mod.rs
│  │  ├─ pages.rs            # HTML (askama)
│  │  ├─ api.rs              # JSON
│  │  └─ media.rs            # /stream, /hls, /subs, /thumb
│  └─ templates/             # askama templates
├─ static/                   # css, js (htmx, hls.js), ikon
├─ tests/
│  ├─ api_tests.rs
│  └─ fixtures/
└─ data/                     # runtime (di-.gitignore): app.db, cache/, hls/, thumbs/
```

---

## 6. Konfigurasi (`config.toml`)

```toml
[server]
host = "0.0.0.0"       # "127.0.0.1" jika hanya lokal
port = 8080

[library]
paths = ["D:/Anime", "/mnt/media/anime"]
extensions = ["mkv", "mp4", "webm", "avi", "m4v"]
scan_on_startup = true
watch = true

[data]
dir = "./data"

[ffmpeg]
ffmpeg_path = "ffmpeg"
ffprobe_path = "ffprobe"
hls_segment_seconds = 6
max_concurrent_transcodes = 2

[metadata]
anilist_enabled = false   # default offline
```

Aturan: config dimuat saat startup, divalidasi (path ada, ffmpeg bisa dijalankan), error jelas jika gagal.
Env var override dengan prefix `ANIME_` (mis. `ANIME_SERVER__PORT=9000`).

---

## 7. Skema Database (SQLite)

```sql
-- migrations/0001_init.sql
CREATE TABLE anime (
  id            INTEGER PRIMARY KEY,
  title         TEXT NOT NULL,
  title_native  TEXT,
  synopsis      TEXT,
  poster_path   TEXT,
  year          INTEGER,
  status        TEXT,                  -- airing / finished / unknown
  anilist_id    INTEGER UNIQUE,
  folder_path   TEXT UNIQUE,           -- folder sumber (boleh NULL untuk entri manual)
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE genre (
  id   INTEGER PRIMARY KEY,
  name TEXT NOT NULL UNIQUE
);

CREATE TABLE anime_genre (
  anime_id INTEGER NOT NULL REFERENCES anime(id) ON DELETE CASCADE,
  genre_id INTEGER NOT NULL REFERENCES genre(id) ON DELETE CASCADE,
  PRIMARY KEY (anime_id, genre_id)
);

CREATE TABLE episode (
  id            INTEGER PRIMARY KEY,
  anime_id      INTEGER NOT NULL REFERENCES anime(id) ON DELETE CASCADE,
  season        INTEGER NOT NULL DEFAULT 1,
  number        REAL    NOT NULL,      -- REAL agar mendukung 12.5 (recap/OVA)
  title         TEXT,
  file_path     TEXT NOT NULL UNIQUE,
  file_size     INTEGER,
  duration_sec  REAL,
  video_codec   TEXT,
  audio_codec   TEXT,
  container     TEXT,
  streams_json  TEXT,                  -- hasil ffprobe ringkas (audio/sub tracks)
  thumb_path    TEXT,
  file_mtime    INTEGER,
  added_at      TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_episode_anime ON episode(anime_id, season, number);

CREATE TABLE progress (
  episode_id   INTEGER PRIMARY KEY REFERENCES episode(id) ON DELETE CASCADE,
  position_sec REAL NOT NULL DEFAULT 0,
  watched      INTEGER NOT NULL DEFAULT 0,
  updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE subtitle (
  id         INTEGER PRIMARY KEY,
  episode_id INTEGER NOT NULL REFERENCES episode(id) ON DELETE CASCADE,
  lang       TEXT,
  label      TEXT,
  format     TEXT NOT NULL,            -- srt / vtt / ass
  file_path  TEXT,                     -- NULL jika embedded
  stream_idx INTEGER                   -- index stream jika embedded
);
```

---

## 8. Parsing Nama File

Contoh pola yang harus didukung (tulis unit test untuk **semua** ini):

```
[SubsPlease] Frieren - 12 (1080p) [A1B2C3D4].mkv      -> Frieren, S1, E12
Frieren S01E12.mkv                                     -> Frieren, S1, E12
Frieren - 12v2.mkv                                     -> Frieren, S1, E12
Jujutsu Kaisen - S02E05 - Title.mp4                    -> Jujutsu Kaisen, S2, E5
Anime Name/Season 2/Anime Name - 03.mkv                -> ambil season dari nama folder
One Piece - 1085.mkv                                   -> One Piece, S1, E1085
Show - 12.5.mkv                                        -> E12.5
```

Aturan:
- Buang tag di dalam `[]` dan `()` (release group, resolusi, CRC) sebelum parsing judul.
- Fallback: jika episode tidak terdeteksi, urutkan berdasarkan nama file dan tandai "perlu dicek".
- Judul anime default dari **nama folder induk** bila lebih bisa diandalkan daripada nama file.
- Hasil parser murni (pure function) → mudah dites.

---

## 9. Endpoint

### Halaman (HTML)
| Method | Path | Fungsi |
|---|---|---|
| GET | `/` | Beranda: "Lanjutkan menonton" + grid anime |
| GET | `/anime/:id` | Detail anime + daftar episode |
| GET | `/watch/:episode_id` | Halaman pemutar |
| GET | `/add` | Form tambah anime manual |
| GET | `/settings` | Pengaturan library & scan |

### API JSON
| Method | Path | Fungsi |
|---|---|---|
| GET | `/api/anime?q=&genre=&page=` | List/cari anime |
| POST | `/api/anime` | Tambah anime manual |
| PATCH | `/api/anime/:id` | Edit metadata |
| DELETE | `/api/anime/:id` | Hapus dari library (bukan hapus file) |
| POST | `/api/anime/:id/episodes` | Tambah episode manual (path file / upload) |
| POST | `/api/library/scan` | Mulai scan; balas `job_id` |
| GET | `/api/library/scan/:job_id` | Status/progres scan |
| POST | `/api/progress/:episode_id` | Simpan posisi `{position_sec, watched}` |
| POST | `/api/anime/:id/refresh-metadata` | Ambil ulang dari AniList |

### Media
| Method | Path | Fungsi |
|---|---|---|
| GET | `/stream/:episode_id` | Direct play, **mendukung `Range`** (206 Partial Content) |
| GET | `/hls/:episode_id/master.m3u8` | Playlist HLS (on-demand) |
| GET | `/hls/:episode_id/:segment` | Segmen HLS |
| GET | `/subs/:subtitle_id.vtt` | Subtitle (dikonversi ke WebVTT) |
| GET | `/thumb/:episode_id.jpg` | Thumbnail |
| GET | `/poster/:anime_id` | Poster |

---

## 10. Detail Implementasi Penting

### 10.1 Direct play dengan Range
- Parse header `Range: bytes=start-end`, balas `206` dengan `Content-Range`, `Accept-Ranges: bytes`, `Content-Length`, `Content-Type` sesuai ekstensi.
- Gunakan `tokio::fs::File` + `seek` + `ReaderStream` (atau `tower_http::services::ServeFile`) agar tidak memuat file ke memori.
- Validasi: `416 Range Not Satisfiable` bila di luar batas.

### 10.2 Keamanan path (WAJIB)
- Client **tidak pernah** mengirim path file; hanya `episode_id`. Path diambil dari DB.
- Saat menambah path manual, `canonicalize()` lalu pastikan berada di dalam salah satu `library.paths` (cegah path traversal/symlink escape).
- Jangan menyajikan file selain ekstensi yang diizinkan.
- Default bind `127.0.0.1`; peringatkan di log jika `0.0.0.0` dipakai tanpa auth.
- Opsional: auth sederhana (password tunggal + cookie session) untuk mode LAN.

### 10.3 Scanner
- Walk direktori (`walkdir` atau `tokio::fs::read_dir` rekursif), filter ekstensi.
- Upsert berdasarkan `file_path`; skip jika `file_mtime` & `file_size` tidak berubah.
- Hapus/tandai episode yang file-nya sudah tidak ada.
- Jalankan `ffprobe` dengan batas konkurensi (mis. 4) menggunakan `tokio::sync::Semaphore`.
- Laporkan progres (jumlah file dipindai/total) lewat channel/atomic untuk endpoint status job.

### 10.4 HLS on-demand
- Tentukan apakah perlu transcode: cek codec dari ffprobe. H.264 + AAC/MP3 di MP4 → direct play. Selainnya → HLS.
- Perintah dasar (remux bila memungkinkan, transcode bila tidak):
  ```
  ffmpeg -i INPUT -map 0:v:0 -map 0:a:0 -c:v libx264 -preset veryfast -crf 20 \
         -c:a aac -b:a 192k -f hls -hls_time 6 -hls_playlist_type event \
         -hls_segment_filename seg_%05d.ts index.m3u8
  ```
- Kelola sesi: satu proses ffmpeg per episode aktif, batasi `max_concurrent_transcodes`, hentikan proses setelah idle (mis. 2 menit), bersihkan cache di folder `data/hls/`.
- Dukung seek jauh: mulai ffmpeg dari offset (`-ss`) bila segmen yang diminta belum ada.

### 10.5 Subtitle
- `.srt` → konversi ke WebVTT (ganti koma jadi titik pada timestamp, tambah header `WEBVTT`).
- Embedded: `ffmpeg -i INPUT -map 0:s:N -f webvtt out.vtt` (cache hasilnya).
- `.ass/.ssa`: konversi ke VTT (kehilangan styling) atau gunakan JASSUB di player untuk tampilan penuh.
- Deteksi file subtitle eksternal dengan nama sama di folder yang sama (`Episode.id.srt`, `Episode.en.ass`).

### 10.6 Progres tonton
- Player kirim `POST /api/progress/:id` tiap ~10 detik dan saat `pause`/`ended`.
- `watched = true` bila posisi ≥ 90% durasi.
- Beranda menampilkan "Lanjutkan menonton" dari progres terbaru yang belum selesai.

### 10.7 Metadata AniList (opsional)
- Query GraphQL `Media(search: $title, type: ANIME)`.
- Cache respons & poster ke `data/cache/`; hormati rate limit (±90 req/menit), gunakan backoff.
- Selalu bisa dilewati; entri manual tidak boleh ditimpa tanpa konfirmasi (flag `metadata_locked`).

---

## 11. Frontend

- Server-rendered dengan Askama + HTMX; CSS ringan (boleh Pico.css/Tailwind CDN-free lokal).
- Semua aset (htmx, hls.js) **di-vendor ke `static/`** supaya jalan tanpa internet.
- Player: `<video controls>` + hls.js jika `canPlayType('application/vnd.apple.mpegurl')` tidak tersedia dan sumber HLS.
- Shortcut keyboard: spasi (play/pause), ←/→ (±5 dtk), `F` (fullscreen), `N` (episode berikutnya).
- Desain responsif (grid poster 2–6 kolom), mode gelap default.

---

## 12. Alternatif UI (opsional)

Jika ingin SPA: pisahkan `frontend/` (Svelte/React + Vite) yang memakai API JSON di §9, dan layani build statis dari axum. Atau gunakan **Leptos**/**Dioxus** untuk full-Rust. Untuk v0.1 tetap disarankan Askama + HTMX demi kesederhanaan.

---

## 13. Rencana Kerja per Milestone (urutan eksekusi untuk Codex)

Kerjakan **satu milestone per sesi/PR**. Setiap milestone harus `cargo fmt`, `cargo clippy -- -D warnings`, dan `cargo test` hijau sebelum selesai.

### M0 — Scaffold
- `cargo new anime-stream`, tambahkan dependensi, struktur folder §5.
- `config.rs` + `config.example.toml`, `tracing`, `error.rs`, `AppState`.
- Route `/health` → `200 OK`.
- **Selesai bila:** `cargo run` menyala dan `/health` bisa diakses.

### M1 — Database & model
- Migrasi §7, modul `db/*` dengan fungsi CRUD dasar, pool SQLite WAL mode.
- Test CRUD memakai SQLite in-memory.
- **Selesai bila:** test DB lulus.

### M2 — Parser & scanner
- `parser.rs` dengan test untuk semua contoh §8.
- `scanner.rs` + endpoint `POST /api/library/scan` dan status job.
- **Selesai bila:** folder fixture berisi file dummy terdeteksi dan masuk DB dengan benar.

### M3 — UI dasar
- Template beranda, detail anime, halaman watch (tanpa progress).
- Form tambah anime manual.
- **Selesai bila:** dapat menelusuri library lewat browser.

### M4 — Streaming Range
- `/stream/:id` dengan Range + test (200, 206, 416, ekstensi salah, id tidak ada).
- Player memakai `/stream/:id`; seek bekerja.
- **Selesai bila:** video MP4/H.264 dapat diputar dan di-seek.

### M5 — Progres & lanjutkan
- Endpoint progress, "Lanjutkan menonton", auto-next, tandai watched.

### M6 — ffprobe, thumbnail, subtitle
- `probe.rs`, simpan `streams_json`, thumbnail, subtitle eksternal & embedded → VTT, pilih track di UI.

### M7 — HLS on-demand
- Keputusan direct play vs HLS, manajer sesi ffmpeg, cleanup, batas konkurensi.

### M8 — Metadata & watcher
- AniList client + cache, tombol refresh, `notify` watcher, debounce.

### M9 — Polish & rilis
- Auth opsional, pengaturan di UI, README, CI (GitHub Actions khusus runner `windows-latest`: fmt, clippy, test), build rilis Windows saja.

---

## 14. Aturan untuk Codex (Working Agreement)

1. **Baca dokumen ini dulu** sebelum mengubah kode; ikuti struktur §5 dan urutan §13.
2. Kerjakan hanya milestone yang diminta; jangan menambah fitur di luar scope tanpa diminta.
3. Jangan `unwrap()`/`expect()` di kode non-test. Gunakan `?` dan `AppError`.
4. Setiap fungsi publik yang punya logika (parser, range, konversi subtitle) **wajib punya unit test**.
5. Jalankan sebelum menyatakan selesai:
   ```
   cargo fmt --all
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```
6. Jangan hardcode path; semua lewat `config.rs`.
7. Jangan pernah menerima path file dari client (lihat §10.2).
8. Perubahan skema DB hanya lewat file migrasi baru, jangan mengedit migrasi lama.
9. Komit kecil dan bermakna (Conventional Commits: `feat:`, `fix:`, `test:`, `refactor:`).
10. Bila ada ambiguitas, pilih opsi paling sederhana, tulis asumsinya di deskripsi PR/komentar, lalu lanjut.
11. Dependensi baru: pilih crate populer & terawat, jelaskan alasannya singkat.
12. Jangan menambahkan fitur scraping/pengunduhan dari situs pihak ketiga.

---

## 15. Strategi Testing

- **Unit:** parser nama file, konversi SRT→VTT, parser header Range, logika keputusan direct-play vs HLS.
- **Integrasi:** jalankan router dengan `axum-test`, DB SQLite sementara, folder fixture dengan file video kecil (beberapa detik, dibuat dengan ffmpeg `testsrc`).
- **Manual (checklist):**
  - [ ] Seek ke tengah/akhir video berfungsi.
  - [ ] Subtitle tampil dan bisa dipilih.
  - [ ] Progres tersimpan & "Lanjutkan" benar.
  - [ ] File MKV HEVC/FLAC jatuh ke HLS dan tetap bisa diputar.
  - [ ] Menambah file baru ke folder muncul otomatis (watcher).
  - [ ] Percobaan `../../etc/passwd` ditolak.

---

## 16. Perintah Berguna

```bash
# Dev
cargo run
cargo watch -x run            # perlu cargo-watch
sqlx migrate run              # perlu sqlx-cli

# Kualitas
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test

# Buat video uji 10 detik
ffmpeg -f lavfi -i testsrc=duration=10:size=640x360:rate=24 \
       -f lavfi -i sine=frequency=440:duration=10 \
       -c:v libx264 -c:a aac -shortest tests/fixtures/Test_Anime/Test_Anime_-_01.mp4

# Release
cargo build --release
```

---

## 17. Prompt Awal yang Disarankan untuk Codex

Salin ke Codex untuk memulai:

> Baca `AGENTS.md`. Kerjakan **M0 — Scaffold** persis sesuai spesifikasi:
> buat proyek Rust dengan axum + tokio + sqlx (SQLite) + tracing + askama,
> struktur folder sesuai §5, loader konfigurasi `config.toml` (§6), `AppError`,
> `AppState`, dan route `/health`. Tambahkan test untuk `/health`.
> Pastikan `cargo fmt`, `cargo clippy -- -D warnings`, dan `cargo test` lulus.
> Jangan mengerjakan milestone lain. Ringkas hasil dan asumsi di akhir.

Prompt lanjutan (pola yang sama): *"Lanjut ke M1 sesuai AGENTS.md"*, dst.

---

## 18. Definisi "Selesai" untuk v1.0

- Semua milestone M0–M9 selesai dan terdokumentasi di README.
- Dapat menambahkan anime baru lewat scan folder maupun form manual.
- Streaming lancar di Chrome/Firefox/Edge/Safari untuk format umum, HLS fallback untuk format lain.
- Berjalan sepenuhnya offline (metadata online opsional).
- Tidak ada `unwrap()` di jalur request; clippy bersih; test lulus di CI.
