# Plan: Tracking pemakaian token AI (per user, per platform, dan total)

> **Status 2026-11-14:** dibangun, dengan satu perubahan dari rencana ini: datanya di **database D1
> sendiri** (`aina-usage`, binding `USAGE`), bukan di `badi-cloud`, dan ditambah tabel per panggilan
> (`ai_calls`, 180 hari) serta leaderboard. Cara pakai: [usage-analytics.md](usage-analytics.md).

**Tujuan:** bisa menjawab dengan angka sungguhan, bukan estimasi dari harga di kode:

1. Berapa **token AI** (bukan kredit aina) yang dipakai **tiap user** dan **totalnya**, per hari/bulan.
2. Dipisah **per platform dan model**: Anthropic (Opus, Sonnet, Haiku), OpenAI (transcribe, TTS,
   realtime), ElevenLabs.
3. Berapa **biaya API sungguhan (HPP, rupiah/USD)** di balik itu, dan berapa **kredit** yang dipotong.
4. Dari situ: **1 kredit yang dipakai user = berapa token** per model, dan margin per paket.

**Prinsip:** angka yang dipakai untuk keputusan bisnis harus **pasti** (tidak di-*sample*), **permanen**
(tidak hilang setelah 3 bulan), dan bisa di-JOIN dengan `users`/`payments`. Tidak menyimpan prompt,
screenshot, audio, atau jawaban (sesuai halaman privasi).

---

## Kondisi sekarang

| Yang ada | Di mana | Cukup? |
|---|---|---|
| Satu baris per panggilan AI: user, route, model, token (5 angka), biaya, latensi | Analytics Engine `aina_ai_calls` (`cloud/src/track.rs`) | Datanya ada, tapi **di-sample** saat trafik besar, **hilang setelah 3 bulan**, tidak bisa JOIN ke D1, dan butuh API token Cloudflare untuk query. |
| Rekap harian `ai_calls`, `ai_credits` per route+model | D1 `daily_stats` (`badi-cloud-core/src/stats.rs`) | Permanen, tapi **tanpa token** dan **tanpa per user**. |
| Saldo kredit per user | D1 `users.sub_left`, `users.topup` | Hanya saldo, tanpa riwayat (lihat `docs/issues/0001-credit-ledger.md`). |

### Masalah yang ditemukan di data sekarang

1. **`double8` bukan kredit, tapi milli-rupiah.** `docs/analytics.md` dan `track.rs` menyebutnya
   "kredit × 1000", padahal yang dikirim adalah `meter.cost()`, `stt_cost()`, `tts_cost()` dan
   `realtime_cost()`, semuanya `Cost` (milli-rupiah). Konversi ke kredit baru terjadi di `Db::spend`
   (`c.to_credits(cost)`). Akibatnya metrik `ai_credits` di `daily_stats` sebenarnya **rupiah HPP**,
   bukan kredit. (Kebetulan itu justru angka yang dibutuhkan untuk HPP; hanya namanya yang salah.)
2. **Platform tidak tercatat.** Harus ditebak dari route (`messages` → Anthropic, `eleven` →
   ElevenLabs, sisanya OpenAI).
3. **Satuan tercampur di kolom yang sama.** `double3` berisi token untuk Claude, karakter untuk TTS, dan
   byte audio untuk transcribe. Tidak bisa dijumlah begitu saja.
4. **Realtime kehilangan rincian.** Hanya `input_tokens`/`output_tokens` total yang disimpan. Token audio
   (jauh lebih mahal) dan teks tidak dibedakan, dan cache tidak tercatat.
5. **Biaya kalimat gratis tidak tercatat.** `voice_line` dikirim dengan `cost = 0` karena user tidak
   dipotong kredit, padahal aina tetap membayar ElevenLabs/OpenAI.
6. **Kredit yang benar-benar dipotong tidak tercatat di mana pun per panggilan.** Akun `unlimited`
   tidak dipotong sama sekali, dan pembulatan ke atas terjadi di `to_credits`.

---

## Desain

### 1. Satu catatan pemakaian yang lengkap per panggilan (`Usage`)

Di `badi-cloud-core`, satu struct yang dihasilkan oleh setiap route sebelum menulis ke mana pun:

```rust
pub struct Usage {
    pub provider: &'static str,   // "anthropic" | "openai" | "elevenlabs"
    pub route: &'static str,      // messages, transcribe, speech, eleven, voice_line, realtime
    pub model: String,
    // Token (0 kalau bukan model token)
    pub input_tokens: u64,        // teks/gambar masuk, di luar cache
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub audio_in_tokens: u64,     // realtime
    pub audio_out_tokens: u64,    // realtime
    // Satuan non-token
    pub web_searches: u64,
    pub audio_ms: u64,            // transcribe: durasi audio
    pub chars: u64,               // TTS / ElevenLabs
    // Uang
    pub cost_milli_idr: i64,      // HPP sungguhan: harga provider × kurs, SELALU diisi (juga untuk yang gratis)
    pub credits_milli: i64,       // yang benar-benar dipotong dari saldo (0 untuk unlimited / kalimat gratis)
}
```

Semua fungsi harga (`claude_cost`, `stt_cost`, `tts_cost`, `realtime_cost`, `search_cost`) tetap
dipakai untuk `cost_milli_idr`. `credits_milli` diisi oleh `Db::spend`, yang diubah agar
**mengembalikan** jumlah kredit yang benar-benar dipotong.

### 2. Tabel permanen di D1: `ai_usage_daily`

Rekap **per user × hari (WIB) × platform × model × route**, ditulis langsung saat panggilan selesai
(bukan dari Analytics Engine), jadi angkanya pasti dan tidak di-sample.

```sql
-- migrations/0011_ai_usage.sql
CREATE TABLE IF NOT EXISTS ai_usage_daily (
  day TEXT NOT NULL,              -- '2026-11-14' (WIB)
  user_id INTEGER NOT NULL,
  provider TEXT NOT NULL,         -- anthropic | openai | elevenlabs
  model TEXT NOT NULL,
  route TEXT NOT NULL,
  calls INTEGER NOT NULL DEFAULT 0,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read_tokens INTEGER NOT NULL DEFAULT 0,
  cache_write_tokens INTEGER NOT NULL DEFAULT 0,
  audio_in_tokens INTEGER NOT NULL DEFAULT 0,
  audio_out_tokens INTEGER NOT NULL DEFAULT 0,
  web_searches INTEGER NOT NULL DEFAULT 0,
  audio_ms INTEGER NOT NULL DEFAULT 0,
  chars INTEGER NOT NULL DEFAULT 0,
  cost_milli_idr INTEGER NOT NULL DEFAULT 0,   -- HPP sungguhan
  credits_milli INTEGER NOT NULL DEFAULT 0,    -- kredit yang dipotong
  PRIMARY KEY (day, user_id, provider, model, route)
);
CREATE INDEX IF NOT EXISTS ai_usage_user ON ai_usage_daily (user_id, day);

-- Nama metrik lama yang salah: isinya rupiah HPP, bukan kredit.
UPDATE daily_stats SET metric = 'ai_cost_idr' WHERE metric = 'ai_credits';
```

Penulisan: satu `INSERT … ON CONFLICT DO UPDATE SET calls = calls + 1, input_tokens = input_tokens +
?, …`, dijalankan dalam **satu `db.batch()` bersama `UPDATE users` di `Db::spend`**, jadi pemotongan
kredit dan catatan pemakaian tidak bisa selisih.

**Kenapa rekap harian, bukan satu baris per panggilan:**

| | Satu baris per panggilan | Rekap harian (dipilih) |
|---|---|---|
| Volume (1.000 user × 200 call/hari) | ±70 juta baris/tahun, mendekati batas 10 GB D1 | ±1.000 × 5 model × 365 ≈ 1,8 juta baris/tahun |
| Pertanyaan "per user / per platform / total" | Bisa | Bisa |
| Riwayat tiap panggilan untuk komplain | Bisa | Tidak (itu tugas ledger, `0001-credit-ledger.md`, yang menyimpan model + token di `meta`) |
| Write per panggilan | 1 INSERT | 1 UPSERT (sama) |

Ledger (issue 0001) dan tabel ini saling melengkapi: ledger untuk **saldo dan audit per panggilan**,
`ai_usage_daily` untuk **laporan pemakaian dan HPP**.

### 3. Analytics Engine tetap, dengan kolom yang dibetulkan

`aina_ai_calls` tetap dipakai untuk latensi, error, dan analisis cepat. Perubahan di `track.rs`:

| Kolom | Sebelum | Sesudah |
|---|---|---|
| `double8` | "kredit × 1000" (sebenarnya milli-rupiah) | **milli-rupiah HPP** (dokumentasi dibetulkan, nilainya tetap) |
| `double11` | — | kredit × 1000 yang dipotong |
| `double12`, `double13` | — | token audio in / out (realtime) |
| `double14`, `double15` | — | `audio_ms`, `chars` (supaya `double3` hanya token) |
| `blob9` | — | provider |

Data lama tetap bisa dibaca (kolom lama tidak berubah arti, kecuali dokumentasi `double8`).

### 4. Laporan (admin API, dibaca dari D1)

Semua di belakang auth admin yang sudah ada.

- `GET /admin/usage?from=2026-11-01&to=2026-11-30&by=provider|model|user`
  Total per kelompok: `calls`, semua kolom token, `cost_idr`, `credits`, dan turunannya:
  - `tokens_per_credit = (input + output + cache_read + cache_write) / credits` per model,
    yaitu jawaban sungguhan "1 kredit = berapa token".
  - `idr_per_credit_real = cost_idr / credits`, harus ≈ 170. Kalau jauh di bawah, ada pembulatan atau
    akun gratis yang perlu dilihat.
- `GET /admin/usage/user?email=…&from=…&to=…`: pemakaian satu user per hari per model.
- `GET /admin/usage/top?from=…&to=…&limit=20`: user dengan token/HPP terbesar (cek abuse, akun
  unlimited).

Contoh query di balik `by=provider`:

```sql
SELECT provider, model,
  SUM(calls) AS calls,
  SUM(input_tokens) AS input, SUM(output_tokens) AS output,
  SUM(cache_read_tokens) AS cache_read, SUM(cache_write_tokens) AS cache_write,
  SUM(audio_in_tokens + audio_out_tokens) AS audio_tokens,
  SUM(chars) AS chars, SUM(audio_ms) / 60000.0 AS audio_minutes,
  SUM(cost_milli_idr) / 1000.0 AS hpp_idr,
  SUM(credits_milli) / 1000.0 AS credits
FROM ai_usage_daily
WHERE day BETWEEN ?1 AND ?2
GROUP BY provider, model ORDER BY hpp_idr DESC;
```

Total semua user: query yang sama tanpa `GROUP BY`. Per user: `GROUP BY user_id` (JOIN `users`
untuk email).

### 5. Mengisi data lama (backfill)

Satu kali, dari Analytics Engine (masih menyimpan 3 bulan terakhir), lewat `POST
/admin/usage/backfill`: query `aina_ai_calls` per hari, dikelompokkan per `index1` (user), `blob1`,
`blob2`, ditulis ke `ai_usage_daily`. Catatan:

- Angkanya **perkiraan** (Analytics Engine bisa men-sample, pakai `SUM(_sample_interval * …)`).
- `double3` untuk transcribe/TTS adalah byte/karakter: dipindah ke `audio_ms` / `chars` saat backfill.
- `credits_milli` tidak ada di data lama: dihitung ulang dari `double8` dengan `to_credits` (akun
  unlimited dianggap 0).
- Hari yang di-backfill ditandai, misalnya baris `daily_stats` metric `usage_backfilled`, supaya
  laporan bisa menyebut "perkiraan" untuk rentang itu.

---

## Pertanyaan yang bisa dijawab langsung sekarang (tanpa menunggu ini dibangun)

Dengan API token Cloudflare (izin **Account Analytics: Read**), dari `aina_ai_calls`. Ingat:
`double8` = milli-**rupiah**.

```sql
-- Token AI per user dan per model, 30 hari
SELECT index1 AS user_id, blob1 AS route, blob2 AS model,
  SUM(_sample_interval) AS calls,
  SUM(_sample_interval * double3) AS input,      -- untuk transcribe = byte audio, speech/eleven = karakter
  SUM(_sample_interval * double4) AS output,
  SUM(_sample_interval * double5) AS cache_read,
  SUM(_sample_interval * double6) AS cache_write,
  SUM(_sample_interval * double8) / 1000 AS hpp_idr
FROM aina_ai_calls
WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob5 = 'ok'
GROUP BY user_id, route, model
ORDER BY hpp_idr DESC
```

Total semua user: query yang sama tanpa `index1` di `SELECT`/`GROUP BY`. Kredit ≈ `hpp_idr / 170`.

---

## Backlog

Ukuran: **S** ≤ 1 jam, **M** ≤ setengah hari.

- [ ] **S** Betulkan dokumentasi `double8` di `track.rs` dan `docs/analytics.md` (milli-rupiah).
- [ ] **M** `Usage` di `badi-cloud-core` + test: tiap route mengisi provider, satuan yang benar,
      `cost_milli_idr` (juga untuk `voice_line`), rincian audio realtime.
- [ ] **S** `Db::spend` mengembalikan `credits_milli` yang dipotong (0 untuk unlimited).
- [ ] **M** Migration `0011_ai_usage.sql` + UPSERT `ai_usage_daily` dalam satu `batch()` dengan
      pemotongan saldo.
- [ ] **S** Kolom baru di `track.rs` (`blob9`, `double11`–`double15`).
- [ ] **M** Admin API `GET /admin/usage`, `/admin/usage/user`, `/admin/usage/top` + entri di
      `docs/aina-admin.postman_collection.json`.
- [ ] **M** Backfill 3 bulan dari Analytics Engine.
- [ ] **S** Rename metrik `ai_credits` → `ai_cost_idr` di `stats.rs` dan `daily_stats`.

## Yang perlu diperhatikan

- **Writes D1:** satu UPSERT tambahan per panggilan, dalam batch yang sama dengan `UPDATE users` yang
  sudah ada, jadi tidak menambah round-trip.
- **Akun unlimited dan kalimat gratis** tetap tercatat dengan `cost_milli_idr` sungguhan dan
  `credits_milli = 0`, supaya HPP total tidak kelihatan lebih kecil dari kenyataan.
- **Kurs dan harga provider** masih konstanta di `plans.rs` (`IDR_PER_USD = 18.000`). `cost_milli_idr`
  menyimpan hasil hitungan saat itu, jadi kalau harga/kurs berubah, data lama tidak ikut berubah.
  Token mentah tetap ada untuk dihitung ulang kalau perlu.
- **Hapus akun (UU PDP):** baris `ai_usage_daily` untuk user yang dihapus disimpan dengan `user_id`
  saja (tanpa email), sama seperti `payments`, atau digabung ke `user_id = 0`.
- **Privasi:** hanya angka (token, karakter, durasi, biaya). Tidak ada isi percakapan.

## Selesai bila

- `GET /admin/usage?by=provider` menampilkan token, HPP rupiah, dan kredit per platform/model untuk
  rentang tanggal apa pun, termasuk lebih dari 3 bulan ke belakang.
- `GET /admin/usage?by=user` menampilkan token per user dan totalnya.
- `tokens_per_credit` per model bisa dibaca langsung dari laporan.
- HPP dari `ai_usage_daily` cocok (±1%) dengan tagihan Anthropic/OpenAI/ElevenLabs untuk bulan yang sama.
