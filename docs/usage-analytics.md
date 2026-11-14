# Analitik pemakaian AI (`aina-usage`)

Database D1 tersendiri untuk menjawab pertanyaan bisnis tentang AI aina dengan angka pasti:

- Seberapa sering aina membantu user, dan seberapa sering user memakai AI-nya?
- Berapa **token** (bukan kredit aina) yang dipakai per user dan totalnya, per platform dan model?
- Berapa **biaya API sungguhan** (HPP) dibanding uang yang masuk, alias **boncos atau tidak**?
- Model mana yang mahal, lambat, atau sering gagal (bahan pertimbangan pindah ke OpenRouter)?
- Leaderboard untuk menyemangati user.

**API docs (Swagger):** buka `docs/api/index.html` di browser (cukup double-click). Sumbernya
`docs/api/usage-openapi.yaml`; setelah mengubahnya, jalankan `python docs/api/build.py`.

Kode: `cloud/src/usage.rs` (D1), `cloud/src/track.rs` (pencatatan), `crates/badi-cloud-core/src/usage.rs`
(aturan, dites). Skema: `cloud/migrations-usage/`.

## Kenapa database sendiri

| | `badi-cloud` (akun & billing) | `aina-usage` (analitik) |
|---|---|---|
| Isi | user, saldo, pembayaran, token login | pemakaian AI per panggilan dan per hari, leaderboard |
| Data pribadi | email, nama, kartu | **tidak ada**: user hanya berupa angka `user_id` |
| Bisa diekspor/dipindah sendiri | tidak disarankan | ya: SQLite biasa, tanpa JSON/trigger |

Ekspor kapan saja: `wrangler d1 export aina-usage --remote --output aina-usage.sql`, lalu buka dengan SQLite,
DuckDB, Metabase, atau impor ke Postgres/BigQuery. Untuk menghubungkan `user_id` ke email, JOIN dengan
`users` dari `badi-cloud` (admin API di bawah sudah melakukannya).

## Satuan

- Uang disimpan dalam **ribuan** supaya tidak hilang karena pembulatan: `cost_milli_idr / 1000` = rupiah,
  `credits_milli / 1000` = kredit aina.
- `cost_milli_idr` = yang **aina bayar** ke provider (harga di `plans.rs` × Rp 18.000/US$), selalu diisi,
  juga untuk akun unlimited dan kalimat suara gratis. `credits_milli` = yang **dipotong dari user**.
- Hari dan jam dalam **WIB**.
- "Bantuan" (`helps`) = panggilan sukses ke Claude (`messages`) atau balasan realtime (`realtime`). Satu
  pertanyaan suara biasanya = transkrip + Claude + suara, tapi dihitung satu bantuan. Agent yang berjalan
  beberapa langkah dihitung per langkah ke Claude.

## Tabel

| Tabel | Isi | Disimpan |
|---|---|---|
| `ai_calls` | satu baris per panggilan AI (berhasil, ditolak, atau gagal): waktu, jam, user, provider, route, model, fitur app (`x-aina-kind`), versi app, hasil, token (input, output, cache read/write, audio in/out), web search, durasi audio, karakter, HPP, kredit, latensi | 180 hari (cron harian menghapus yang lebih tua) |
| `ai_usage_daily` | total per hari × user × provider × model × route × fitur | permanen |
| `leaderboard` | user yang **memilih ikut** leaderboard dan nama yang ditampilkan | sampai user keluar atau hapus akun |

Route → provider: `messages` → `anthropic`; `transcribe`, `speech`, `realtime`, `realtime_session` →
`openai`; `eleven` → `elevenlabs`; `voice_line` → `elevenlabs` atau `openai` sesuai mesin suaranya.

## View siap pakai

| View | Pertanyaan |
|---|---|
| `v_daily` | per hari: user aktif, jumlah bantuan, panggilan, token, HPP, kredit |
| `v_models_monthly` | per bulan × provider × model: token, HPP, kredit, gagal, rata-rata ms |
| `v_users` | per user sepanjang waktu: hari pertama/terakhir, hari aktif, bantuan, token, HPP, kredit |
| `v_features_monthly` | per bulan × fitur app: berapa user dan panggilan |
| `v_hours` | jam berapa (WIB) orang memakai aina (180 hari terakhir) |

```powershell
npx wrangler d1 execute aina-usage --remote --command "SELECT * FROM v_daily ORDER BY day DESC LIMIT 14"
```

## Admin API

Semua butuh `Authorization: Bearer <ADMIN_TOKEN>`. Rentang: `?from=2026-11-01&to=2026-11-30` (hari WIB,
inklusif) atau `?days=30` (default).

| Endpoint | Isi |
|---|---|
| `GET /admin/usage/summary` | total, margin (pendapatan vs HPP), per provider, per model |
| `GET /admin/usage/daily` | deret harian (grafik) |
| `GET /admin/usage/users?sort=cost\|helps\|tokens&limit=50` | per user + email, paket, pendapatan, margin |
| `GET /admin/usage/user?email=…` | satu user per hari per model + marginnya |
| `GET /admin/usage/features` | per fitur app |
| `GET /admin/usage/hours` | per jam WIB |

Setiap baris berisi jumlah mentah (`calls_ok`, `calls_failed`, semua kolom token, `chars`,
`web_searches`, `helps`, `users`, `active_days`) dan turunan:

| Field | Arti |
|---|---|
| `tokens` | total semua jenis token |
| `costIdr`, `costUsd` | HPP |
| `credits` | kredit yang dipotong |
| `idrPerCredit` | HPP sungguhan per kredit; harus ≈ 170. Jauh di atas 170 = ada pemakaian yang tidak dipotong (akun unlimited) |
| `tokensPerCredit` | **1 kredit = berapa token** untuk kelompok itu |
| `failRate`, `avgMs` | tingkat gagal dan rata-rata waktu per panggilan sukses |
| `margin` | `revenueIdr`, `costIdr`, `grossProfitIdr`, `grossMargin` (0-1), `loss` (true = boncos) |

`revenueIdr` = pembayaran berstatus `paid` di `badi-cloud.payments` yang `paid_at`-nya di rentang itu.
Catatan: top-up dibeli sekarang bisa dipakai bulan depan, jadi margin per bulan bisa naik turun. Lihat
juga rentang panjang (`?days=365`).

## Menjawab pertanyaan bisnis

**Seberapa sering aina membantu?** `GET /admin/usage/daily` → `helps` dan `users` per hari;
`v_users.active_days` untuk retensi (berapa hari user benar-benar memakai).

**Token per user dan total?** `GET /admin/usage/users?sort=tokens` dan `total.tokens` di summary. Rincian
per jenis token ada di kolom masing-masing.

**Boncos pakai Claude atau tidak?** `GET /admin/usage/summary` → `margin.loss`, dan `byProvider` untuk
porsi HPP Anthropic. Per user: `GET /admin/usage/users` → `margin.loss = true` (biasanya akun gratis atau
unlimited, atau pemakai berat).

**Bukti untuk pindah ke OpenRouter?** `byModel` di summary (atau `v_models_monthly`): token, HPP,
`avgMs`, dan `failRate` per model. Bandingkan HPP per model dengan harga OpenRouter untuk jumlah token yang
sama; `failRate` dan `avgMs` menunjukkan apakah provider sekarang bermasalah. Contoh:

```sql
SELECT model,
  SUM(input_tokens) AS input, SUM(output_tokens) AS output,
  SUM(cache_read_tokens) AS cache_read, SUM(cache_write_tokens) AS cache_write,
  SUM(cost_idr) AS hpp_idr, SUM(calls_failed) * 1.0 / (SUM(calls_ok) + SUM(calls_failed)) AS fail_rate
FROM v_models_monthly WHERE provider = 'anthropic' AND month >= '2026-11'
GROUP BY model ORDER BY hpp_idr DESC;
```

Dikali harga per juta token di OpenRouter = berapa biayanya di sana. (Cache read di Anthropic jauh lebih
murah; cek apakah OpenRouter meneruskan prompt caching untuk model yang sama.)

## Leaderboard (untuk app)

Hanya user yang **memilih ikut** yang tampil, dengan nama yang mereka setujui. Diurutkan menurut jumlah
bantuan, lalu jumlah hari aktif.

| Endpoint | Isi |
|---|---|
| `GET /v1/leaderboard?period=week\|month\|all` | `top` (20 teratas: `rank`, `name`, `helps`, `days`, `me`), `me` (`joined`, `name`, `helps`, `days`, `rank`), `suggestedName` |
| `PUT /v1/leaderboard { "name": "Sari" }` | ikut / ganti nama (2-24 huruf). Tanpa `name`: nama depan + inisial dari Google ("Sari W.") |
| `DELETE /v1/leaderboard` | keluar |

`week` = 7 hari WIB terakhir, `month` = 30 hari terakhir. User yang belum ikut tetap melihat angkanya
sendiri (`me.helps`) tanpa peringkat. Hapus akun juga mengeluarkan dari leaderboard.

## Deploy

1. Database sudah dibuat (`aina-usage`, APAC) dan migrasinya sudah diterapkan:
   `npx wrangler d1 migrations apply aina-usage --remote`.
2. Migrasi `badi-cloud` 0011 (ganti nama metrik `ai_credits` → `ai_cost_idr` di `daily_stats`):
   `npx wrangler d1 migrations apply badi-cloud --remote`.
3. `npx wrangler deploy` dari `cloud/`.

Tanpa binding `USAGE`, Worker tetap jalan; pencatatan ke `aina-usage` dan endpoint di atas saja yang mati.

## Privasi

Hanya angka (token, karakter, durasi, biaya, waktu). Tidak ada prompt, screenshot, audio, atau jawaban,
dan tidak ada email/nama di `aina-usage` kecuali nama leaderboard yang dipilih user sendiri. Saat akun
dihapus, baris pemakaiannya tetap ada di bawah `user_id` saja (tidak bisa dikaitkan ke orangnya lagi
karena barisnya di `users` ikut terhapus), dan namanya di leaderboard dihapus.
