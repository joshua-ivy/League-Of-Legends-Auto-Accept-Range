# chud-skins — upstream-source.dev catalog cache Worker

Caches the [upstream-source.dev](https://upstream-source.dev) mod catalog so Chud clients query
**our** cache, never upstream-source directly. Used **with upstream-source's permission**
(the maintainers, 2026-07-12) on the conditions:

- **Do not spike their usage / DDoS them** — hence the gentle daily crawl + full self-hosting of images and (optionally) files.
- **Do not link the app to their website** (drives cost).
- **Credit the mod author** (each skin carries its `publisher`), plus a plain-text "via upstream-source.dev".

## Endpoints
- `GET /catalog?search=&champion=&category=&page=&pageSize=` → filtered, paginated (mods carry `id,name,champions,category,thumb,publisher,downloads,likes`).
- `GET /img/{thumbnailKey}` → self-hosted mirrored image (Cloudflare cache, + R2 if bound).
- `GET /download/{modId}[?redirect=1]` → resolves the `.fantome` URL; if an R2 `FILES` bucket is bound, mirrors it and serves from us.
- `GET /file/{modId}` → streams a mirrored `.fantome` from our R2.
- `GET /meta` → `{count, crawledAt, crawlProgress}`.
- `GET /crawl?key=CRAWL_KEY[&full=1]` → manual spurt / full seed (guarded).

## Gentle-load design
- Cron `*/15 * * * *`: each run grabs `CHUNK_PAGES` (3) pages, advancing a cursor; once the day's catalog is assembled it idles until tomorrow. Upstream-source sees ~3 page fetches / 15 min, never a burst.
- Images mirrored on first view (once per image, ever).
- Download URLs resolved on first download and cached 7 days.

## Resilience (survive upstream-source blocking us)
Bind an R2 bucket as `FILES` (and optionally `IMAGES`) — then downloads mirror into our R2 and serve from us, so if upstream-source ever cuts access, everything we've mirrored still works. R2 has **zero egress fees** (ideal for serving skin files). Requires R2 enabled on the account. Without R2 the worker still runs (catalog + images-via-cache; downloads fall back to upstream-source's direct URL).

## Deploy
```
npx wrangler kv namespace create CATALOG   # once; put id in wrangler.toml
npx wrangler secret put CRAWL_KEY          # guards /crawl
npx wrangler deploy
curl "https://chud-skins.<sub>.workers.dev/crawl?key=SECRET&full=1"  # initial seed
```

Deployed: `https://chud-skins.jivy26.workers.dev`
