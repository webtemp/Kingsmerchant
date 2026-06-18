# Recorded API fixtures

Offline fixtures for the `trade-api` tests. Nothing here hits the network.

| File | Provenance |
|---|---|
| `data_stats.json` | **Real capture** — a curated subset of `GET https://www.pathofexile.com/api/trade2/data/stats` (2026-06, Mirage league). Trimmed to the stat entries the tests exercise; the JSON shape (groups → entries with `id`/`text`/`type`) is verbatim. |
| `data_items.json` | **Real capture** — curated subset of `GET .../api/trade2/data/items`. Keeps a handful of accessory/weapon bases plus their uniques (note `Mageblood` is a `Utility Belt`, used to test base splitting). |
| `search_response.json` | Hand-built to the `POST .../api/trade2/search/{league}` response shape, with **controlled values** so the deterministic median/cheapest tests have known inputs (5 ids, total 137). |
| `fetch_response.json` | Hand-built to the `GET .../api/trade2/fetch/{ids}` response shape. Five Topaz Ring listings spanning exalted/divine prices, an afk seller, an offline seller, and a null-price listing — engineered so median = 3 exalted and cheapest-N drops the unpriced one. |
| `search_response_real.json` | **Real capture** — `POST search/Runes of Aldur` (Topaz Ring), result list trimmed to 10. Anonymous search works fine; an earlier 400 was just an invalid league id, not a missing session. |
| `fetch_response_real.json` | **Real capture** — `GET fetch/{ids}` for the first five of that search. Includes live currencies (`aug`, `regal`) and a non-ASCII whisper, so the serde models are proven against real data. |
| `scout_leagues.json` | Trimmed to the real poe2scout `GET /poe2/Leagues` shape — the league name is the `Value` field, alongside `DivinePrice`/`IsCurrent`. The current league is flagged so the Divine-rate name-match + `IsCurrent` fallback are both exercised. |
| `scout_currency.json` | To the real poe2scout `GET /poe2/Leagues/{Value}/Currencies/{ApiId}` shape (`Preserved Cranium`, priced in Exalted): `ApiId`/`Text`/`CurrentPrice`/`PriceLogs` with a leading `null` price-log entry (as the live API returns), so the parser is proven to skip gaps when computing the recent low/high. Note the `ApiId` is the official `data/static` exchange id, not the slugified name, for orbs (`divine`, `exalted`, …). |

The real `X-Rate-Limit-*` header values used by the rate-limit tests
(`5:10:60,15:60:300,30:300:1800` and the `…-state` companion) were likewise
captured from a live `search` response and are embedded directly in the tests.

Valid POE2 trade league ids come from `GET .../api/trade2/data/leagues`
(currently `Runes of Aldur`, `HC Runes of Aldur`, `Standard`, `Hardcore`) — the
plain `/api/leagues?realm=poe2` endpoint returns POE1 leagues and must not be
used for trade.
