# Analytics: traction dan tracing AI

Dua dataset di Workers Analytics Engine (akun Cloudflare yang sama dengan Worker):

| Dataset | Ditulis oleh | Isi |
|---|---|---|
| `aina_events` | `aina-web/worker.js` (binding `EVENTS`) | kunjungan halaman dan download installer dari aina.ai.id |
| `aina_ai_calls` | `cloud/src/track.rs` (binding `AI_CALLS`) | satu baris per panggilan AI lewat proxy |

Yang **tidak** dicatat: prompt, screenshot, audio, jawaban, IP, cookie. Pengunjung web dihitung dengan hash
harian (tanggal + `VISITOR_SALT` + IP + browser), jadi orang yang sama dihitung sekali per hari dan tidak
bisa diikuti dari hari ke hari.

Data disimpan 3 bulan. Analytics Engine bisa men-*sample* saat trafik besar, jadi hitungan selalu pakai
`SUM(_sample_interval)`, bukan `COUNT()`.

## Cara query

1. Buat API token: Cloudflare dashboard → My Profile → API Tokens → Create Token → Custom, izin
   **Account → Account Analytics → Read**.
2. Jalankan SQL:

```powershell
$env:CF_TOKEN = "<token>"
$q = "SELECT blob1 AS event, SUM(_sample_interval) AS n FROM aina_events WHERE timestamp > NOW() - INTERVAL '7' DAY GROUP BY event"
curl.exe -s "https://api.cloudflare.com/client/v4/accounts/d4d0be3bd86e0bc6cbfea92a6bd3c86e/analytics_engine/sql" `
  -H "Authorization: Bearer $env:CF_TOKEN" --data $q
```

## Kolom `aina_events`

| Kolom | Isi |
|---|---|
| `index1`, `blob10` | id pengunjung (hash harian, 16 hex) |
| `blob1` | `pageview` atau `download` |
| `blob2` | path (`/`, `/playground`, `/api/download`, …) |
| `blob3` | situs asal (referrer di luar aina.ai.id), mis. `linkedin.com`; kosong = langsung |
| `blob4` | `utm_source` (atau `ref`); untuk download diambil dari halaman tempat tombol diklik |
| `blob5`, `blob6` | `utm_medium`, `utm_campaign` |
| `blob7` | negara (kode 2 huruf) |
| `blob8` | `desktop`, `mobile`, `tablet` |
| `blob9` | `1` = bot/crawler (saring dengan `blob9 = '0'`) |
| `blob11` | OS: `windows`, `mac`, `android`, `ios`, `linux`, `other` |
| `double2` | HTTP status (download berhasil = 200) |

## Kolom `aina_ai_calls`

| Kolom | Isi |
|---|---|
| `index1`, `blob8` | id akun (`users.id` di D1) |
| `blob1` | route: `messages`, `transcribe`, `speech`, `eleven`, `realtime_session`, `realtime` |
| `blob2` | model |
| `blob3` | fitur dari app (`x-aina-kind`: `tanya`, agent, …) |
| `blob4` | versi app (`x-aina-version`, terisi mulai app 1.0.2) |
| `blob5` | hasil: `ok`, `refused` (ditolak aturan aina Cloud), `upstream` (provider error), `error` (provider tidak terjangkau) |
| `blob6` | alasan penolakan (`plan_limit` = kredit habis/batas paket, `rate_limit_error`, `invalid_request_error`, …) |
| `blob7` | `stream` atau `whole` |
| `double2` | HTTP status |
| `double3`–`double7` | token input, output, cache read, cache write, web search (audio: `double3` = karakter atau byte) |
| `double8` | biaya dalam kredit × 1000 |
| `double9` | ms sampai byte/token pertama dari provider (0 = tidak diketahui) |
| `double10` | ms total |

## Query siap pakai

**Funnel 30 hari: pengunjung → pengunjung yang download**

```sql
SELECT
  COUNT(DISTINCT index1) AS pengunjung,
  COUNT(DISTINCT if(blob1 = 'download' AND double2 = 200, index1, NULL)) AS pengunduh
FROM aina_events
WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob9 = '0'
```

(`COUNT(DISTINCT …)` tidak memperhitungkan sampling; akurat selama trafik belum puluhan ribu per hari.)

**Per hari**

```sql
SELECT toStartOfDay(timestamp) AS hari,
  SUM(if(blob1 = 'pageview', _sample_interval, 0)) AS kunjungan,
  SUM(if(blob1 = 'download' AND double2 = 200, _sample_interval, 0)) AS download
FROM aina_events
WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob9 = '0'
GROUP BY hari ORDER BY hari
```

**Dari mana pengunjung datang**

```sql
SELECT if(blob4 != '', blob4, if(blob3 != '', blob3, '(langsung)')) AS sumber,
  SUM(_sample_interval) AS kunjungan
FROM aina_events
WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob1 = 'pageview' AND blob9 = '0'
GROUP BY sumber ORDER BY kunjungan DESC LIMIT 20
```

**Download per OS** (yang bukan Windows belum bisa memakai installer)

```sql
SELECT blob11 AS os, SUM(_sample_interval) AS n
FROM aina_events
WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob1 = 'download' AND blob9 = '0'
GROUP BY os ORDER BY n DESC
```

**AI: pemakaian, biaya, dan latensi per model (7 hari)**

```sql
SELECT blob1 AS route, blob2 AS model,
  SUM(_sample_interval) AS panggilan,
  SUM(_sample_interval * double8) / 1000 AS kredit,
  quantileWeighted(0.5)(double9, _sample_interval) AS p50_first_ms,
  quantileWeighted(0.95)(double10, _sample_interval) AS p95_total_ms
FROM aina_ai_calls
WHERE timestamp > NOW() - INTERVAL '7' DAY AND blob5 = 'ok'
GROUP BY route, model ORDER BY panggilan DESC
```

**AI: error dan penolakan**

```sql
SELECT blob5 AS hasil, blob6 AS alasan, double2 AS status, SUM(_sample_interval) AS n
FROM aina_ai_calls
WHERE timestamp > NOW() - INTERVAL '7' DAY AND blob5 != 'ok'
GROUP BY hasil, alasan, status ORDER BY n DESC
```

**AI: user aktif harian dan user paling boros**

```sql
SELECT toStartOfDay(timestamp) AS hari, COUNT(DISTINCT index1) AS user_aktif
FROM aina_ai_calls WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob5 = 'ok'
GROUP BY hari ORDER BY hari
```

```sql
SELECT index1 AS user_id, SUM(_sample_interval) AS panggilan, SUM(_sample_interval * double8) / 1000 AS kredit
FROM aina_ai_calls WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob5 = 'ok'
GROUP BY user_id ORDER BY kredit DESC LIMIT 20
```

**Pemakaian per fitur app**

```sql
SELECT blob3 AS fitur, SUM(_sample_interval) AS panggilan, COUNT(DISTINCT index1) AS user
FROM aina_ai_calls WHERE timestamp > NOW() - INTERVAL '30' DAY AND blob5 = 'ok'
GROUP BY fitur ORDER BY panggilan DESC
```

## Rekap harian permanen (D1 `daily_stats`)

Analytics Engine hanya menyimpan 3 bulan, jadi cron harian Worker (08:00 WIB) menyalin angka hari
kemarin (hari WIB) ke tabel D1 `daily_stats`: satu baris per (hari, metrik, dimensi). Hari yang terlewat
(cron gagal, query gagal) dicoba lagi otomatis selama masih dalam 7 hari, satu hari per jalan (batas 50 subrequest di Workers Free); `POST /admin/stats/roll` berulang kali untuk mengejar lebih cepat. Daftar metrik dan query-nya ada
di `crates/badi-cloud-core/src/stats.rs`:

| Metrik | Dimensi |
|---|---|
| `visits`, `visitors`, `downloads`, `downloaders`, `bot_hits` | — |
| `visits_by_source`, `downloads_by_source` | sumber (utm / situs asal / `(langsung)`) |
| `visits_by_page`, `visits_by_country`, `visits_by_device`, `downloads_by_os` | path / negara / device / OS |
| `ai_users` | — |
| `ai_calls`, `ai_credits`, `ai_first_ms_p50` | `route:model` |
| `ai_calls_by_feature` | fitur (`x-aina-kind`) |
| `ai_problems` | `hasil:alasan:status` |

Butuh `CF_ACCOUNT_ID` (sudah di `wrangler.toml`) dan secret `CF_ANALYTICS_TOKEN` (token dengan izin Account
Analytics: Read): `wrangler secret put CF_ANALYTICS_TOKEN`. Tanpa secret itu rekap dilewati.

Admin API (header `Authorization: Bearer <ADMIN_TOKEN>`):

- `GET /admin/stats?days=30` → `{ "from": "...", "days": { "2026-11-09": { "visits": { "": 120 }, "visits_by_source": { "linkedin": 80 } } } }`
- `POST /admin/stats/roll` → jalankan rekap sekarang; `?redo=1` mengulang 7 hari terakhir.

Pindah dari Cloudflare: `wrangler d1 export badi-cloud --remote --table daily_stats --output daily_stats.sql`
memberi SQL biasa (SQLite) yang bisa dimuat ke database lain.
