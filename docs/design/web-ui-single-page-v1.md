# Web UI V1 (Single-Page, No-Build)

## Purpose
Define the v1 UX contract for `DBX64P` under the locked architecture:
- one static page (`/ui/index.html`)
- vanilla JS + inline CSS
- no React/framework runtime
- no frontend build step

## Users
- Product managers watching an agent run in real time
- QA engineers watching test event streams

## URL Model
- `/ui` shows pool list.
- `/ui/pools/{name}` shows watch view for that pool.
- Deep links are shareable and should restore pool context on load.

## Screens

### 1) Pool List (`/ui`)

```text
+--------------------------------------------------------------+
| PLASMITE UI                              status: connected   |
|--------------------------------------------------------------|
| Pools                                                        |
| [search omitted in v1]                                       |
|                                                              |
| > orders-prod                  P: 10000   newest: #9912      |
| > qa-smoke                     P: 2048    newest: #2047      |
| > agent-thoughts               P: 8192    newest: #777       |
|                                                              |
| Tap/click a pool to watch live stream                        |
+--------------------------------------------------------------+
```

Behavior:
- Sorted by pool name ascending.
- Row click navigates to `/ui/pools/{name}`.
- If no pools exist, show empty state with plain guidance.

### 2) Pool Watch (`/ui/pools/{name}`)

```text
+--------------------------------------------------------------+
| < Back     pool: qa-smoke                                    |
|--------------------------------------------------------------|
| N: 400 buffered in browser   P: 2048 pool cap   live: yes    |
|--------------------------------------------------------------|
| {"seq": 1641, "topic":"run", "status":"start"}         |
| {"seq": 1642, "topic":"run", "status":"ok"}            |
| {"seq": 1643, "topic":"assert", "name":"x"}            |
| ...                                                          |
| {"seq": 2047, "topic":"summary", "result":"pass"}      |
+--------------------------------------------------------------+
```

Behavior:
- Newest messages render at bottom.
- Auto-scroll is on when user is already near bottom.
- If user scrolls up, freeze auto-scroll until they return near bottom.
- SSE reconnect uses backoff and shows `live: reconnecting` then `live: yes`.

## N/P Semantics
- `P` is total pool message capacity from server metadata.
- `N` is client scrollback window in memory for this tab/session.
- Default `N` for v1: `400` messages.
- Hard cap `N`: `2000` messages to prevent runaway browser memory usage.
- If messages exceed `N`, drop oldest client-side entries (ring behavior).

## Mobile Layout
- Single column on narrow viewports.
- Header indicators wrap to two lines if needed.
- Message font scales down slightly but remains monospace/readable.
- Tap targets in pool list use comfortable row height.

## Data/Render Contract
- Message payloads render as escaped text (not HTML insertion).
- JSON messages are pretty-printed only if fast enough; fallback is compact JSON lines.
- Binary/non-JSON payloads display metadata plus safe preview text.

## Errors/States
- Missing pool in URL: show not-found state and link back to `/ui`.
- Auth failure: show explicit auth-required message.
- SSE disconnect: show reconnecting state without clearing existing `N` buffer.

## Out-of-Scope (V1)
- Filtering/search
- Pause/resume controls
- Multi-pool parallel view
- Persistent user settings/local storage
