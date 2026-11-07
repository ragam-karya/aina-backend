# Badi Cloud (Cloudflare Workers, Rust)

Akun berlangganan untuk B.A.D.I.:
- Login dengan Google di browser. Setelah berhasil, browser membuka `badi://` dan app langsung masuk.
- 10 kredit gratis sekali per PC.
- Kredit: langganan Basic/Pro (30 hari, sisa hangus di akhir siklus) dan Top-Up Small/Medium/Large (tidak hangus). Daftar harga di tabel `plans` (`migrations/0003_credits.sql`), 1 kredit = Rp170 biaya API. Tidak ada batas sesi atau mingguan.
- Pembayaran lewat Midtrans (Snap). Langganan yang dibayar kartu kredit menyimpan kartu di Midtrans (Badi hanya memegang `saved_token_id`) dan diperpanjang otomatis oleh cron harian worker (08:00 WIB, `/v2/charge` dengan token; ditolak → dicoba lagi tiap hari sampai 3x dengan masa tenggang 3 hari). User membatalkan/mengaktifkan lagi/menghapus kartu di Settings → Akun (`/v1/subscription/cancel|resume`, `DELETE /v1/subscription/card`). QRIS, VA, e-wallet: sekali bayar.
  - Dashboard Midtrans → Settings → Configuration: *Payment Notification URL* = `https://<worker>/v1/billing/midtrans`. Notifikasi yang hilang tetap tertangani: order `pending` dicek ke `/v2/{order}/status` saat app membaca akun dan oleh cron.
  - **Langganan dari web** (tanpa app): halaman `/masuk`, `/langganan`, `/terima-kasih` ada di website (`FRONTEND_URL`, repo `aina-web`), yang memanggil Worker ini lewat proxy `/api/*` (service binding). Alur: `aina.ai.id/masuk` → `/api/login?next=/langganan` (set cookie nonce `badi_login`) → Google → `PUBLIC_URL/auth/google/callback` (tidak berubah) → `aina.ai.id/api/auth/finish?code=…` (kode sekali pakai + nonce yang sama) → cookie sesi `badi_session` (HttpOnly, Secure, SameSite=Lax; token `bct_` berlabel `web`) di domain website → `GET /v1/account`, `GET /v1/plans`, `POST /v1/checkout` → Snap → `aina.ai.id/terima-kasih?order_id=…` (`callbacks.finish` per transaksi) → `GET /v1/orders/{id}`. Path lama di Worker (`/masuk`, `/langganan`, `/terima-kasih`) redirect ke website. Kredit gratis tetap hanya dari login pertama lewat app di PC. Panggilan bercookie selain GET wajib `Origin` = `PUBLIC_URL` atau `FRONTEND_URL`.
  - **Installer di R2**: sekali `wrangler r2 bucket create badi-downloads`, lalu tiap rilis:
    `wrangler r2 object put badi-downloads/windows/latest --remote --file "../target/release/bundle/nsis/B.A.D.I._<versi>_x64-setup.exe" --content-type application/octet-stream --content-disposition "attachment; filename=\"B.A.D.I._<versi>_x64-setup.exe\""`
  - **Production:** minta Midtrans mengaktifkan fitur simpan kartu (One Click / tokenisasi kartu) untuk merchant, lalu `MIDTRANS_PRODUCTION = "1"` dan server key production.
- Proxy yang memegang key Anthropic, OpenAI, ElevenLabs dan Composio.

Aturannya ada di `../crates/badi-cloud-core` (dites native: `cargo test -p badi-cloud-core`). Folder ini hanya lem HTTP + D1 (worker-rs → WebAssembly).

Data yang disimpan: email, paket, angka biaya pemakaian, pembayaran, hash token, kepemilikan session Composio. Pertanyaan, suara, screenshot dan jawaban hanya lewat, tidak disimpan.

## Alur login

1. App → browser: `GET /login?app_state=<acak 64 hex>`.
2. Worker → Google (scope `openid email`, `prompt=select_account`).
3. Google → `GET /auth/google/callback`. Worker menukar `code`, membaca email yang terverifikasi dari ID token, lalu membuat akun (dengan trial) bila belum ada.
4. Worker membuka `badi://login?code=<sekali pakai, 5 menit>&state=<app_state>`.
5. App (instance yang sudah jalan menerima link lewat single-instance) memeriksa bahwa `state` adalah miliknya, lalu `POST /v1/auth/exchange`. Hasilnya token perangkat `bct_…`, yang disimpan di Windows Credential Manager.

## Deploy pertama (sandbox)

```powershell
npm i -g wrangler
rustup target add wasm32-unknown-unknown
wrangler login
wrangler d1 create badi-cloud                     # salin database_id ke wrangler.toml
wrangler d1 migrations apply badi-cloud --remote
wrangler deploy                                   # catat URL-nya: https://badi-cloud.<akun>.workers.dev
```

Isi `PUBLIC_URL` (URL di atas) dan `GOOGLE_CLIENT_ID` di `wrangler.toml`, lalu secret:

```powershell
wrangler secret put GOOGLE_CLIENT_SECRET
wrangler secret put ANTHROPIC_API_KEY
wrangler secret put OPENAI_API_KEY
wrangler secret put ELEVENLABS_API_KEY
wrangler secret put COMPOSIO_API_KEY       # project key ak_…
wrangler secret put COMPOSIO_AUTH_CONFIGS  # opsional: {"gmail":"ac_…",…} supaya login aplikasi kerja atas nama B.A.D.I.
wrangler secret put MIDTRANS_SERVER_KEY    # SB-Mid-server-… (sandbox); MIDTRANS_PRODUCTION = "1" nanti untuk live
wrangler deploy
```

**Google OAuth client** (Google Cloud Console → APIs & Services → Credentials → Create OAuth client ID → Web application):
- Authorized redirect URI: `https://badi-cloud.<akun>.workers.dev/auth/google/callback`
- Consent screen: scope `openid` dan `email` saja (tidak butuh verifikasi Google untuk scope ini). Nama aplikasi B.A.D.I., beserta link `/privasi` dan `/ketentuan` dari Worker ini.

**Midtrans sandbox:** Dashboard → Settings → Payment → Notification URL: `https://badi-cloud.<akun>.workers.dev/v1/billing/midtrans`.

**App:** isi `cloudUrl` di config, atau `BADI_CLOUD_URL` saat dev, dengan URL Worker.

## Lokal

```powershell
wrangler d1 migrations apply badi-cloud --local
wrangler dev                                      # http://localhost:8787 (redirect URI lokal: http://localhost:8787/auth/google/callback)
$env:BADI_CLOUD_URL = "http://localhost:8787"; npm run dev
```

## Batas Cloudflare yang perlu diingat
- **CPU per request** (bukan waktu tunggu): pakai paket berbayar $5/bulan. Streaming Claude hampir seluruhnya menunggu jaringan.
- **Body request** maksimal 100 MB, cukup untuk screenshot dan audio.
- **Pencatatan biaya:** dicatat di D1 setelah stream selesai (`ctx.wait_until`), juga saat app memutus di tengah jalan.
