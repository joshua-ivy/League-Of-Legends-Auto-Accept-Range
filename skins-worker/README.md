# chud-skins

Internal Cloudflare Worker: a cached catalog + asset API for the Chud app,
backed by KV + R2.

Endpoints: `/catalog`, `/img/{key}`, `/download/{id}`, `/file/{id}`, `/meta`.

## Deploy
```
npx wrangler deploy
```
Bindings and schedule are in `wrangler.toml`. `CRAWL_KEY` is a wrangler secret.
