# Evidence: BE-01 – BE-03 (audit sebelum migrasi `be.aina.ai.id`)

- **Tanggal:** 2026-11-09
- **Pelaksana:** Claude Code (atas permintaan Faisal)
- **Plan:** [`../plan-custom-domain-be.md`](../plan-custom-domain-be.md)
- **Sifat:** read-only. Tidak ada perubahan di Cloudflare, D1, Composio, atau Google.

| Task | Status | Ringkasan |
|---|---|---|
| BE-01 | ✅ Selesai (1 item perlu dicek manual) | `be.aina.ai.id` belum ada. Zone di Cloudflare plan Free. Request non-browser (GET & POST) lolos tanpa challenge. |
| BE-02 | ✅ Selesai (Google Console dicek manual) | 9 auth config, **semua** memakai callback Composio. Tidak ada yang ke workers.dev, jadi **Fase 4 tidak diperlukan**. |
| BE-03 | ✅ Selesai | **Menghapus auth config = connected account yang memakainya berhenti berfungsi.** BE-15 harus diganti ke PATCH. |

---

## BE-01 · Audit DNS & zone `aina.ai.id`

### 1. DNS
Resolver sistem (`Resolve-DnsName`):

```
be.aina.ai.id A     -> DNS name does not exist
be.aina.ai.id AAAA  -> DNS name does not exist
be.aina.ai.id CNAME -> DNS name does not exist
aina.ai.id     A    104.21.96.52, 172.67.173.71
aina.ai.id     AAAA 2606:4700:3034::ac43:ad47, 2606:4700:3036::6815:6034
www.aina.ai.id A    172.67.173.71, 104.21.96.52
aina.ai.id     NS   lily.ns.cloudflare.com, ace.ns.cloudflare.com
```

**Temuan:** `be` belum dipakai (NXDOMAIN, berarti tidak ada wildcard record). Zone memakai nameserver
Cloudflare, jadi custom domain Worker bisa langsung dibuat.

### 2. Zone & akun Cloudflare
Cloudflare API, token OAuth wrangler (`wrangler whoami` → faisalmashuri16@gmail.com):

```
GET /zones?name=aina.ai.id
  9f18ef01fa59f57c16fd019a43506967  aina.ai.id  active  plan=Free Website  account=d4d0be3bd86e0bc6cbfea92a6bd3c86e

GET /accounts/{acc}/workers/domains
  aina.ai.id     -> aina-landing (production)
  www.aina.ai.id -> aina-landing (production)

GET /accounts/{acc}/workers/scripts/badi-cloud/subdomain
  {"enabled": true, "previews_enabled": true}

GET /zones/{zone}/workers/routes
  []   (tidak ada route lama yang bisa bentrok)
```

**Temuan:**
- Zone dan Worker `badi-cloud` ada di **akun yang sama**, jadi `custom_domain = true` bisa dipakai.
- Belum ada custom domain untuk `badi-cloud`, dan workers.dev-nya aktif.
- `previews_enabled: true`: preview URL juga aktif. Pertahankan dengan menulis eksplisit di
  `wrangler.toml` saat BE-04 (`workers_dev = true`, dan `preview_urls = true` kalau memang dipakai).

### 3. Pengaturan keamanan zone
Token wrangler **tidak punya izin** membaca pengaturan zone, WAF/rulesets, bot management, maupun
DNS records:

```
GET /zones/{zone}/settings                                   -> 9109 Unauthorized
GET /zones/{zone}/bot_management                             -> 10000 Authentication error
GET /zones/{zone}/rulesets/phases/http_request_firewall_custom/entrypoint -> 10000
GET /zones/{zone}/rulesets/phases/http_ratelimit/entrypoint  -> 10000
GET /zones/{zone}/rulesets/phases/http_request_sbfm/entrypoint -> 10000
GET /zones/{zone}/dns_records?name=be.aina.ai.id             -> 10000
```

Sebagai gantinya, aku **uji perilaku** langsung lewat zone `aina.ai.id`. Jalurnya `aina-landing`,
lalu service binding ke `badi-cloud`, jadi request benar-benar melewati WAF/Bot Fight zone yang sama:

```
GET  https://aina.ai.id/api/v1/plans  UA=curl/8              -> 200 application/json
GET  https://aina.ai.id/api/v1/plans  UA="badi-app (reqwest)"-> 200 application/json
GET  https://aina.ai.id/api/v1/plans  UA=Mozilla/5.0         -> 200 application/json
POST https://aina.ai.id/api/v1/auth/exchange  UA=curl/8 body={}
     -> 401 {"type":"error","error":{"type":"authentication_error",...}}   (respons dari Worker, bukan challenge)
GET  https://badi-cloud.faisalmashuri16.workers.dev/v1/plans -> 200 application/json
```

Tidak ada header `cf-mitigated` dan tidak ada halaman challenge di semua respons.

**Temuan:** saat ini zone **tidak menantang** request non-browser, baik GET maupun POST. Dengan
konfigurasi sekarang, app desktop dan webhook Midtrans kemungkinan besar aman lewat
`be.aina.ai.id`.

**Keputusan untuk BE-05:** bypass rule **belum diperlukan**. BE-05 cukup diverifikasi ulang setelah
BE-04, dengan tes yang sama ke `be.aina.ai.id`.

### ⚠️ Perlu dicek manual (dashboard Cloudflare → aina.ai.id)
- [ ] **Security → Bots:** status *Bot Fight Mode*. Di plan Free, mode ini tidak bisa di-skip per
  hostname. Kalau suatu saat dinyalakan, efeknya ke semua subdomain termasuk `be`.
- [ ] **Security → WAF:** custom rules dan rate limiting rules. Catat kalau ada.
- [ ] **Security → Settings:** Security Level dan Browser Integrity Check.

---

## BE-02 · Audit auth config Composio

### Yang sudah dipastikan dari kode
- Auth config white-label disimpan di D1 tabel `billing_settings`, key `composio_auth_configs`
  (JSON `{ toolkit_slug: "ac_…" }`). Fallback-nya secret lama `COMPOSIO_AUTH_CONFIGS`.
- Redirect URI disimpan di Composio, di dalam `credentials.oauth_redirect_uri` tiap auth config
  (`rules::brand_auth_config`). Nilainya `https://badi-cloud.faisalmashuri16.workers.dev/api/composio-redirect`
  kalau didaftarkan dengan `viaBadi: true`, atau callback Composio kalau `viaBadi: false`.
- Provider yang didukung: google, microsoft, slack, github, notion, linear, jira, clickup, hubspot,
  salesforce, dropbox.

### Hasil (2026-11-09, `GET https://backend.composio.dev/api/v3/auth_configs`, API key dari Faisal, read-only)

| auth config ID | Toolkit | Nama | Tipe | Koneksi | `oauth_redirect_uri` |
|---|---|---|---|---|---|
| ac_WaT1bdUiqzna | gmail | B.A.D.I. (gmail) | custom (OAuth app sendiri) | 3 | `https://backend.composio.dev/api/v1/auth-apps/add` |
| ac_BbdKuvouoEif | googlecalendar | B.A.D.I. (googlecalendar) | custom | 2 | sama |
| ac_XBDep5tiKEaf | googledrive | B.A.D.I. (googledrive) | custom | 0 | sama |
| ac_G-wABEOO_jqP | googledocs | B.A.D.I. (googledocs) | custom | 0 | sama |
| ac_I7vyk2d0K9NT | googlesheets | B.A.D.I. (googlesheets) | custom | 0 | sama |
| ac_Ajt6xIkb4kt_ | googletasks | B.A.D.I. (googletasks) | custom | 0 | sama |
| ac_Wo1EGEg00No- | gmail | auth_config_gmail_… | default (Composio managed) | 6 | sama |
| ac_ZiscEWZnFpUL | googlecalendar | auth_config_googlecalendar_… | default (Composio managed) | 2 | sama |
| ac_gGoXQuxwe2UO | googlesheets | auth_config_googlesheets_… | default (Composio managed) | 1 | sama |

**Kesimpulan BE-02:**
- Hanya provider **google** yang punya auth config white-label (6 toolkit, 5 koneksi aktif).
- **Tidak ada** auth config yang memakai `…workers.dev/api/composio-redirect`. Semuanya didaftarkan
  dengan `viaBadi: false` (callback langsung ke Composio).
- Artinya domain backend **tidak berpengaruh** ke OAuth Composio. **Fase 4 (BE-14, BE-15) tidak
  diperlukan**, dan Composio bukan alasan workers.dev harus tetap hidup. BE-08 tetap dikerjakan
  sebagai pengaman supaya pendaftaran berikutnya tidak berubah diam-diam.
- Ada juga auth config Composio-managed (default) dengan 9 koneksi. Itu koneksi lama atau yang
  dibuat di luar white-label, dan tidak terpengaruh migrasi.
- **Risiko BE-19 terkonfirmasi:** menjalankan ulang `POST /admin/composio` untuk `google` akan
  menghapus 6 auth config custom di atas dan memutus **5 koneksi aktif** (gmail 3, googlecalendar 2).

### Catatan awal (sebelum API key tersedia)
Membaca D1 production (`wrangler d1 execute --remote`) **ditolak** oleh guard Claude Code (kategori
*Production Reads*). Data-nya perlu diambil oleh Faisal.

### Perintah untuk dijalankan Faisal
Semuanya read-only. Jalankan dari `aina-backend/cloud`:

```bash
# 1) ID auth config per toolkit
npx wrangler d1 execute badi-cloud --remote \
  --command "SELECT key, value FROM billing_settings WHERE key = 'composio_auth_configs'"

# 2) Ringkasan dari admin API (butuh ADMIN_TOKEN)
curl -s -H "Authorization: Bearer $ADMIN_TOKEN" \
  https://badi-cloud.faisalmashuri16.workers.dev/admin/composio
#    -> redirectUris.viaBadi, providers[].badi, authConfigs

# 3) Redirect URI yang benar-benar tersimpan di Composio, per auth config (butuh COMPOSIO_API_KEY)
curl -s -H "x-api-key: $COMPOSIO_API_KEY" \
  https://backend.composio.dev/api/v3/auth_configs/<ac_id> | jq '.credentials.oauth_redirect_uri // .'
```

Lalu buka console OAuth tiap provider yang muncul (Google Cloud → APIs & Services → Credentials,
Azure App registrations, Slack app, dst) dan catat **Authorized redirect URIs**.

### Template hasil (diisi Faisal)
| Toolkit | auth config ID | `oauth_redirect_uri` di Composio | URI terdaftar di console provider |
|---|---|---|---|
| gmail | | | |
| googlecalendar | | | |
| … | | | |

Kalau semua `oauth_redirect_uri` berisi callback Composio (bukan workers.dev), **Fase 4 tidak
diperlukan**, dan Composio tidak lagi menjadi alasan workers.dev harus tetap hidup.

### Google Sign-In (BE-02b)
- [ ] Google Cloud Console → OAuth client `905981309985-ng04qcnc…` → pastikan Authorized redirect URI
  `https://aina.ai.id/api/auth/google/callback` ada. **Tidak perlu diubah**, karena kode memakai
  `FRONTEND_URL` (`cloud/src/lib.rs:115`).

---

## BE-03 · Apa yang terjadi kalau auth config Composio dihapus

### Temuan dari dokumentasi Composio
1. **Menghapus auth config membuat connected account-nya mati.** SDK reference (`authConfigs.delete`):
   menghapus auth config *"will prevent any connected accounts that use this auth config from
   functioning"* dan *"cannot be undone"*.
2. API v3 `DELETE /auth_configs/{nanoid}` adalah soft delete dan tidak bisa dibatalkan. Dengan
   `?revoke_on_delete=true`, token di sisi provider juga dicabut (default `false`).
3. **Connected account terikat ke auth config asalnya.** Halaman white-labeling: *"Existing connected
   accounts remain tied to their original auth config."*
4. **Ada endpoint PATCH untuk mengubah auth config yang sudah ada:**
   `PATCH /api/v3.1/auth_configs/{nanoid}`. Sejak changelog 2026-01-14, PATCH memakai *true PATCH
   semantics*: hanya field yang dikirim yang berubah, field lain dipertahankan. Contoh resminya
   merotasi `credentials.client_secret` saja.

### Dampak ke kode kita
`POST /admin/composio` (`cloud/src/lib.rs` ±390–420) membuat auth config baru, lalu **menghapus auth
config lama** (`DELETE auth_configs/{old}`). Komentar di kode bilang *"Accounts linked before keep
their old sign-in until they are relinked"*. Menurut dokumentasi di atas, ini **tidak benar**: akun
yang tertaut ke auth config lama akan berhenti berfungsi.

**Ini risiko di production saat ini, terlepas dari migrasi domain.** Menjalankan ulang
`POST /admin/composio` untuk provider yang sudah terdaftar akan memutus koneksi Gmail, Slack, dll.
milik semua user di provider itu.

### Rekomendasi
- **BE-15 diganti:** jangan pakai `POST /admin/composio`. Ubah redirect URI **di tempat** lewat
  `PATCH /api/v3.1/auth_configs/{ac_id}` dengan body yang hanya berisi
  `credentials.oauth_redirect_uri`. ID auth config tetap sama, jadi connected account tetap terikat.
  Urutannya: tambah URI baru di console provider (BE-14), PATCH satu provider, tes link baru dan
  pemakaian akun lama, lalu ulangi untuk provider berikutnya.
- **Item backlog baru (disarankan): BE-19 · Amankan `POST /admin/composio`.** Tolak request kalau
  provider sudah punya auth config, kecuali ada flag eksplisit `replace: true`. Perbaiki juga
  komentarnya.
- Sebelum PATCH di production, uji dulu di project Composio sandbox/dev: pastikan field
  `oauth_redirect_uri` memang bisa di-PATCH (dokumentasi tidak menyebutnya secara eksplisit), dan
  `type` mana yang harus dikirim (`custom` atau `default`, dokumentasinya tidak konsisten).

### Sumber
- [Composio SDK reference – AuthConfigs](https://docs-1rb5ba4bf.preview.composio.dev/reference/sdk-reference/typescript/auth-configs.md)
- [Composio API v3 – Delete auth config](https://docs.composio.dev/reference/v3/api-reference/auth-configs/deleteAuthConfigsByNanoid)
- [Composio API – Update (PATCH) auth config](https://docs.composio.dev/reference/api-reference/auth-configs/patchAuthConfigsByNanoid)
- [Composio changelog 2026-01-14 – True PATCH semantics](https://docs.composio.dev/docs/changelog/2026/1/14)
- [Composio – White-labeling (direct execution)](https://docs.composio.dev/docs/auth-configuration/white-labeling.md)
