# Chud runes Worker

A tiny Cloudflare Worker that powers Chud's **rune / summoner-spell / item-build auto-import**. It fetches the current-patch "best build" from u.gg's public overview JSON, normalizes it to Chud's client contract, and caches it at the edge. **No Riot Web API key is involved.**

## Why a proxy (not fetch straight from the app)
- u.gg blocks header-less requests; the Worker adds a browser `User-Agent`.
- If u.gg ever changes its JSON shape, we fix it **here** — no Chud app update needed.
- One cached origin fetch serves every user and every role.

## Endpoint
```
GET /runes?champ=<championId>&role=<top|jungle|mid|bot|support|"">&sort=<winrate|popular>
```
Returns:
```json
{ "name": "",
  "runes":  { "primary": 8100, "sub": 8200, "perks": [8112,8106,8139,8140,8226,8237,5005,5008,5011] },
  "spells": [4, 14],
  "items":  { "blocks": [ { "type": "Starting", "items": [1056,2003,2003] } ] } }
```
`perks` is always 9 IDs (keystone, 3 primary, 2 secondary, 3 shards) — exactly what the League client's `/lol-perks/v1/pages` wants. Empty `role` (blind pick / ARAM) → the champion's most-played position.

## Deploy (one time)
```bash
cd runes-worker
npx wrangler deploy
```
That prints a URL like `https://chud-runes.<your-subdomain>.workers.dev`. Then in Chud, set:
- `config.runes.endpoint` = `https://chud-runes.<your-subdomain>.workers.dev/runes`
- `config.runes.enabled` = `true`

(Your party relay Worker already proves wrangler is set up for this account.)

## Test it
```bash
curl "https://chud-runes.<your-subdomain>.workers.dev/runes?champ=103&role=mid"
```
Champ 103 = Ahri. You should get a runes/spells/items JSON.

## Data source notes
- Primary source: `stats2.u.gg/lol/1.5/overview/<patch>/ranked_solo_5x5/<champ>/1.5.0.json` (server 12 = all-regions, tier 10 = Platinum+).
- Patch comes from Data Dragon `versions.json`; falls back one minor version if a fresh patch isn't processed yet.
- Fallback source to add later if u.gg ever hard-blocks: `a1.lolalytics.com/mega?ep=rune&...` (no bot protection, but needs a second request for items and has no spell data).
- Caching (6h on the u.gg fetch) keeps us a good citizen and fast. Adjust `cacheTtl` in `src/index.js` if you want fresher data right after a patch.
