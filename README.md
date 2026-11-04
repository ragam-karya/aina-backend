# aina backend

Backend aina: Cloudflare Worker (Rust → WebAssembly) untuk akun, kredit, pembayaran Midtrans, dan proxy yang memegang key AI dan Composio.

- `cloud/` — worker (worker-rs + D1 + R2, dijalankan dengan wrangler). Panduan lengkap: [`cloud/README.md`](cloud/README.md).
- `crates/badi-cloud-core/` — aturan tanpa I/O (paket, token login, cek Midtrans, billing), dipakai worker dan dites native:
  `cargo test -p badi-cloud-core`
- `docs/badi-admin.postman_collection.json` — koleksi Postman untuk admin API.

Secret tidak pernah ada di repo: pasang dengan `wrangler secret put <NAME>` (daftar di `cloud/wrangler.toml`).
