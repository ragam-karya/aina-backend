# Plan: Backend pindah ke `be.aina.ai.id` (URL workers.dev tetap hidup)

**Tujuan:** Worker `badi-cloud` bisa diakses lewat `https://be.aina.ai.id`, sementara
`https://badi-cloud.faisalmashuri16.workers.dev` tetap melayani prod (app desktop lama, webhook,
redirect OAuth Composio yang sudah terdaftar) sampai traffic-nya habis.

**Prinsip:** satu Worker, dua hostname. Tidak ada migrasi data (D1, R2, cron tetap sama). Setiap
langkah bisa di-rollback sendiri-sendiri, dan URL lama baru dimatikan di fase terakhir, setelah ada
datanya.

---

## Ringkasan dampak per integrasi

| Integrasi | Terpengaruh? | Alasan (dari kode) | Aksi |
|---|---|---|---|
| **Google Sign-In** | **Tidak** | Redirect URI dibangun dari `FRONTEND_URL` (`callback_url()`, `cloud/src/lib.rs:115`) → `https://aina.ai.id/api/auth/google/callback`. State login disimpan di D1, jadi host awal `/login` tidak berpengaruh. | Tidak perlu ubah Google Console. Cukup smoke test login app dan web lewat domain baru. |
| **Midtrans** | Kecil | Webhook diverifikasi lewat `signature_key` (`lib.rs:582`), tidak tergantung host. Finish URL Snap pakai `FRONTEND_URL` (`lib.rs:486`). Notification URL cuma satu dan diatur di dashboard. Notifikasi yang hilang tetap diselesaikan oleh cron dan cek status. | Ganti Notification URL di dashboard (opsional). Risiko utamanya WAF/Bot Fight di zone `aina.ai.id` memblokir POST dari Midtrans. |
| **Composio** | **Ya, paling berisiko** | `oauth_redirect_uri` **disimpan permanen di auth config Composio saat dibuat** (`rules::brand_auth_config`, via `POST /admin/composio` dengan `viaBadi: true`). Mengganti `PUBLIC_URL` **tidak** mengubah auth config yang sudah ada. Auth config itu tetap mengarahkan provider ke `workers.dev/api/composio-redirect`. Registrasi ulang akan **menghapus** auth config lama (`lib.rs:~413`). | Selama auth config lama aktif, workers.dev **tidak boleh** dimatikan. Kalau mau dimigrasi, perlu langkah sendiri (lihat Fase 4). |
| Origin check (cookie) | Ya | `lib.rs:62` cuma menerima `PUBLIC_URL` dan `FRONTEND_URL`. | Terima daftar origin (lama + baru). |
| App desktop | Ya | `DEFAULT_URL` di-hardcode saat build (`aina-notch/crates/badi-app/src/cloud.rs:20`). | Rilis build baru dengan `BADI_CLOUD_URL=https://be.aina.ai.id`. App lama tetap pakai workers.dev. |
| aina-web | Tidak | Ke backend lewat service binding (nama Worker). | Hanya merapikan referensi di `tools/build-playground.py`. |

### Catatan Composio (detail)
- `PUBLIC_URL` hanya mempengaruhi **registrasi baru** dan nilai `viaBadi` di `GET /admin/composio`.
- Token refresh Composio tidak memakai redirect URI, jadi koneksi yang sudah tertaut tidak putus
  hanya karena domain berubah.
- Yang perlu diverifikasi ke Composio sebelum registrasi ulang: **apakah menghapus auth config ikut
  memutus connected account yang memakainya?** Komentar di kode bilang "akun yang tertaut sebelumnya
  tetap pakai sign-in lama sampai di-link ulang", tapi kode langsung `DELETE auth_configs/{old}`.
  Ini harus dicek dulu.
- Kalau auth config dibuat dengan `viaBadi: false` (callback langsung ke Composio), domain backend
  sama sekali tidak berpengaruh. Cek dulu provider mana yang `viaBadi`.

---

## Backlog

Ukuran: **S** ≤ 1 jam, **M** ≤ setengah hari.

### Fase 0 — Persiapan & audit (tanpa perubahan prod)

- [ ] **BE-01 (S) Audit DNS & zone `aina.ai.id`**
  Pastikan belum ada record `be`. Catat setting zone yang bisa memblokir non-browser: Bot Fight Mode,
  Super Bot Fight, WAF custom rules, rate limiting, Browser Integrity Check, cache rules untuk `/*`.
  *Selesai jika:* ada daftar setting dan keputusan perlu dibuat bypass/skip rule untuk `be.aina.ai.id`
  atau tidak.

- [ ] **BE-02 (S) Audit auth config Composio**
  Panggil `GET /admin/composio`. Catat provider mana yang terdaftar dan URI redirect yang dipakai
  (`viaBadi` atau callback Composio). Cek di console tiap OAuth app (Google Cloud, Microsoft, Slack,
  dst) URI redirect apa saja yang terdaftar.
  *Selesai jika:* ada tabel provider → redirect URI aktif.

- [ ] **BE-03 (S) Tanya/cek perilaku Composio saat auth config dihapus**
  Docs atau support Composio: apakah connected account ikut mati?
  *Selesai jika:* ada jawaban tertulis. Jawaban ini menentukan nasib Fase 4.

### Fase 1 — Pasang domain baru (tidak mengubah perilaku)

- [ ] **BE-04 (S) Tambah custom domain di `wrangler.toml`**
  ```toml
  workers_dev = true
  routes = [{ pattern = "be.aina.ai.id", custom_domain = true }]
  ```
  `PUBLIC_URL` **belum** diubah.
  *Selesai jika:* `wrangler deploy` sukses, dan `curl https://be.aina.ai.id/v1/plans` serta
  `curl https://badi-cloud.faisalmashuri16.workers.dev/v1/plans` mengembalikan data yang sama.

- [ ] **BE-05 (S) Terapkan hasil BE-01 di zone**
  Buat skip/bypass rule untuk host `be.aina.ai.id` jika Bot Fight/WAF berisiko memblokir app
  desktop atau webhook.
  *Selesai jika:* request dengan UA non-browser (curl, app) dan `POST /v1/billing/midtrans` tidak
  kena challenge (cek Security Events).

- [ ] **BE-06 (S) Log hostname per request**
  Aktifkan Workers Logs (`[observability]`, sampling 10%); URL request (termasuk host) tercatat otomatis, supaya traffic lama vs baru
  kelihatan.
  *Selesai jika:* ada cara melihat jumlah request per host per hari.

### Fase 2 — Backend menerima dua origin

- [ ] **BE-07 (S) Origin check menerima daftar URL**
  Tambah var `ALT_PUBLIC_URLS` (dipisah koma; sekarang `https://be.aina.ai.id`), lalu sertakan di
  `same_origin` (`cloud/src/lib.rs:62`). Tambah unit test di `badi-cloud-core` untuk kedua origin.
  *Selesai jika:* test lulus. POST bercookie dengan `Origin` lama maupun baru diterima, origin asing
  ditolak.

- [ ] **BE-08 (S) Pisahkan URL redirect Composio dari `PUBLIC_URL`**
  Tambah var `COMPOSIO_REDIRECT_BASE` (default = `PUBLIC_URL`) yang dipakai `proxy_uri()`
  (`lib.rs:940`). Dengan begitu `PUBLIC_URL` bisa pindah tanpa ikut memindahkan redirect Composio
  secara tidak sengaja. Isi nilainya dengan URL workers.dev.
  *Selesai jika:* `GET /admin/composio` → `viaBadi` tetap URL workers.dev.

- [ ] **BE-09 (S) Ganti `PUBLIC_URL` ke `https://be.aina.ai.id`**
  Tukar: `ALT_PUBLIC_URLS` diisi URL workers.dev, lalu deploy.
  *Selesai jika:* smoke test BE-10 lulus di kedua host.

- [ ] **BE-10 (M) Smoke test end-to-end di kedua host**
  - Login app (workers.dev dan be.aina.ai.id) → `badi://login` kembali ke app
  - Login web `aina.ai.id/masuk` → `/langganan`
  - Proxy Anthropic, ElevenLabs (stream), Composio dari app
  - `/download`, `/ketentuan`, `/privasi`
  - Checkout sandbox/kecil → `/terima-kasih` → status order terbayar
  *Selesai jika:* semua lulus dan tidak ada error baru di log.

### Fase 3 — Pindahkan klien & integrasi ke domain baru

- [ ] **BE-11 (S) Midtrans: ganti Payment Notification URL**
  Ubah ke `https://be.aina.ai.id/v1/billing/midtrans` (production, dan sandbox kalau dipakai).
  Lakukan setelah BE-05.
  *Selesai jika:* satu transaksi nyata/sandbox menghasilkan notifikasi dengan status 200 di log host
  baru. Jika gagal, rollback = kembalikan URL di dashboard. Cron tetap menyelesaikan order yang
  pending.

- [ ] **BE-12 (S) Build app dengan `BADI_CLOUD_URL=https://be.aina.ai.id`**
  Ubah juga fallback `DEFAULT_URL` di `aina-notch/crates/badi-app/src/cloud.rs:22`, beserta test
  `badi-net` (`aina-notch/crates/badi-net/src/lib.rs:95-98`).
  *Selesai jika:* installer baru dirilis dan app baru terlihat di log host baru.

- [ ] **BE-13 (S) Rapikan referensi URL lama**
  - `aina-notch/crates/badi-app/installer/license.txt` → link Ketentuan/Privasi diarahkan ke
    `aina.ai.id`
  - `aina-backend/docs/badi-admin.postman_collection.json` → `baseUrl`
  - `aina-web/tools/build-playground.py:101`
  - `aina-backend/cloud/README.md` (deploy, Midtrans, Composio)
  - Test di `badi-cloud-core` (`lib.rs:387`, `rules.rs:733`) boleh tetap atau diganti contoh netral
  *Selesai jika:* `grep workers.dev` hanya menyisakan yang memang disengaja.

### Fase 4 — Composio (opsional, tergantung BE-03)

> **Update 2026-11-09 (evidence BE-02):** semua auth config memakai callback Composio, tidak ada
> yang memakai workers.dev. **BE-14 dan BE-15 tidak diperlukan.** BE-19 tetap dikerjakan.

- [ ] **BE-14 (S) Daftarkan URI baru di tiap OAuth app provider**
  Tambahkan `https://be.aina.ai.id/api/composio-redirect` **di samping** URI lama (jangan diganti)
  di console Google, Microsoft, Slack, dll., sesuai hasil BE-02.
  *Selesai jika:* semua console punya dua URI.

> **Update 2026-11-09 (evidence BE-03, `evidence/BE-01-03-audit.md`):** menghapus auth config
> mematikan connected account-nya. BE-15 sekarang memakai `PATCH /api/v3.1/auth_configs/{id}`
> (ubah `credentials.oauth_redirect_uri` di tempat), **bukan** `POST /admin/composio`. Lihat juga
> BE-19.

- [ ] **BE-19 (S) Amankan `POST /admin/composio`**
  Tolak request kalau provider sudah punya auth config, kecuali ada `replace: true`. Perbaiki komentar
  yang menyebut akun lama tetap jalan.
  *Selesai jika:* ada test, dan re-POST tanpa flag mengembalikan 409.

- [ ] **BE-15 (M) Ubah redirect URI auth config per provider (PATCH, bukan registrasi ulang)**
  Uji dulu di project Composio dev. Lalu satu provider (yang paling sedikit user-nya):
  `PATCH /api/v3.1/auth_configs/{ac_id}` dengan body hanya `credentials.oauth_redirect_uri =
  https://be.aina.ai.id/api/composio-redirect`, ubah `COMPOSIO_REDIRECT_BASE`, lalu tes link akun
  baru dan pakai akun lama.
  *Selesai jika:* link baru lewat host baru, akun lama tetap jalan. Ulangi untuk provider lain.
  *Jika BE-03 tidak aman:* tunda. workers.dev tetap hidup untuk redirect Composio, dan itu tidak
  masalah karena biayanya nol.

### Fase 5 — Pensiunkan workers.dev (nanti, berbasis data)

- [ ] **BE-16 (S) Tentukan kriteria shutdown**
  Contoh: traffic workers.dev < 1% selama 30 hari, tidak ada auth config Composio yang masih
  memakainya, dan Midtrans sudah pindah.

- [ ] **BE-17 (M) (Opsional) Paksa update app lama**
  Minimum app version: app lama mendapat respons "silakan update".

- [ ] **BE-18 (S) Matikan workers.dev**
  Set `workers_dev = false`, kosongkan `ALT_PUBLIC_URLS`, lalu hapus URI lama dari console OAuth.
  *Rollback:* set `workers_dev = true`, lalu deploy ulang.

---

## Urutan & dependensi

```
BE-01 ─► BE-04 ─► BE-05 ─► BE-06
BE-02 ─► BE-03 ─────────────────────────────► BE-14 ─► BE-15
              BE-07 + BE-08 ─► BE-09 ─► BE-10 ─► BE-11, BE-12, BE-13
                                                         └─► (data dari BE-06) ─► BE-16 ─► BE-17 ─► BE-18
```

Fase 0–3 bisa selesai dalam 1–2 hari kerja. Fase 4 menunggu jawaban Composio. Fase 5 menunggu data
traffic (minggu–bulan).

## Risiko utama

1. **WAF/Bot Fight memblokir app atau webhook di domain baru** → BE-05, dan verifikasi di BE-10/BE-11.
2. **Registrasi ulang Composio memutus akun user** → BE-03 wajib selesai sebelum BE-15. Jangan
   jalankan `POST /admin/composio` setelah `PUBLIC_URL` diganti sebelum BE-08 selesai.
3. **Mematikan workers.dev terlalu cepat** → app lama dan redirect Composio putus. Ikuti kriteria
   BE-16.
