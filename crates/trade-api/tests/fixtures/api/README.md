# Recorded API fixtures

Offline fixtures for the `trade-api` tests. Nothing here hits the network.

| File | Provenance |
|---|---|
| `data_stats.json` | **Real capture** — a curated subset of `GET https://www.pathofexile.com/api/trade2/data/stats` (2026-06, Mirage league). Trimmed to the stat entries the tests exercise; the JSON shape (groups → entries with `id`/`text`/`type`) is verbatim. |
| `data_items.json` | **Real capture** — curated subset of `GET .../api/trade2/data/items`. Keeps a handful of accessory/weapon bases plus their uniques (note `Mageblood` is a `Utility Belt`, used to test base splitting). |
| `search_response.json` | Hand-built to the documented `POST .../api/trade2/search/{league}` response shape (`id` + `result` ids + `total`). The live search endpoint validates against a session cookie, so the body is synthetic but structurally faithful. |
| `fetch_response.json` | Hand-built to the `GET .../api/trade2/fetch/{ids}` response shape. Five Topaz Ring listings spanning exalted/divine prices, an afk seller, an offline seller, and a null-price listing — enough to test median + cheapest-N + whisper extraction. |

The real `X-Rate-Limit-*` header values used by the rate-limit tests
(`5:10:60,15:60:300,30:300:1800` and the `…-state` companion) were likewise
captured from a live `search` response and are embedded directly in the tests.
