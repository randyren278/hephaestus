# Hephaestus Web Console

A local, read-only web console for the existing daemon protocol. It shows agent comparisons and the evidence behind their measured results. The MCP gateway
(`docs/MCP_GATEWAY.md`) and remote worker execution (`docs/REMOTE_WORKERS.md`)
are separate daemon-adjacent processes, not part of this browser surface;
this console cannot mutate canonical state — it forwards a fixed allowlist
of read-only commands to the daemon and nothing else.

## Launch from an installation

The macOS package includes the console, static assets and Node runtime. A full
source installation builds the console once and uses your Node.js 22+ runtime.

```sh
heph web
# Inspect an existing pilot without starting another daemon:
heph web --data-dir /path/to/pilot/data --no-daemon
```

`heph web` starts a missing daemon frozen with the quickstart source fixture.
It skips the Senate question and terminal tour. It never unfreezes or executes
an agent. Ctrl+C stops the browser server; the daemon keeps running. Stop it
with `heph stop --data-dir /path/to/pilot/data` when finished. Host
`NODE_OPTIONS` and `NODE_PATH` do not affect this launch.

## Run from a source checkout

Requires Node.js 22 or newer and a running Hephaestus daemon.

```sh
cd apps/hephaestus-web
npm ci
npm start
```

Both launch paths print a URL once bound, for example:

```
Hephaestus web console: http://127.0.0.1:47213/#token=<64 hex characters>
Bound to 127.0.0.1 only. Read-only. Ctrl+C to stop.
```

Open that exact URL. `npm start` builds the browser assets before starting the
server, so a source checkout needs no separate build step. The `#token=...` fragment is a fresh 256-bit value
generated for this one launch; the browser never sends a URL fragment to any
server, so the page reads it once client-side and attaches it as a header on
every API call afterward. The token is then removed from the address bar and
kept in session storage for this tab and origin, so reloading stays connected.
It is never saved to local storage. An expired token is cleared and the page
asks you to open the URL from the newly started console. The **Refresh** button
reloads the current view; connection failures show a retry message. Opening a
fresh printed URL in the same tab also reconnects without a manual reload. A non-default data directory can be selected with
`HEPHAESTUS_HOME=/path/to/data npm start`, matching the [TUI](../hephaestus-tui/README.md).
A fixed port can be requested with `HEPHAESTUS_WEB_PORT=4200 npm start`; by
default the server picks a random port and retries a few times if it is
taken.

## What it shows

- **Status**: `frozen`, active run count, canonical event count, registered
  Genome count, and a job lookup by ID.
- **Worlds**: every registered World, each drawn as its own bordered card
  with a color derived from its World ID so unrelated Worlds are never
  visually confused with one another. Each card shows its Genome lineage
  grouped by Champion role (Champion, standby, quarantined) and its full
  Champion transition history.
- **Genome**: one Genome's record, its role in its World's Champion
  projection, and a line-level diff of its verified Markdown prompt against
  its first registered parent's prompt.
- **Genes**: every extracted Gene with its World, lineage, positive/neutral/
  negative transfer counts, contradiction flag, and species count.
- **Drift & Canary**: recent drift records (World, kind, observed delta vs.
  threshold) and recent canaries (candidate, World, stage, transition count).
  Read-only; drift never replaces a Champion and this view cannot start,
  advance, or roll back anything.
- **Experiments**: registered Evolver strategies (`EvolverStrategyConfig`)
  and recorded meta-evaluation receipts, including each receipt's bootstrap
  quality/cost delta intervals.
- **Evidence & activity**: paired comparisons with parent/candidate visible
  correctness, cost and latency, the correctness change and bootstrap interval,
  and independent measured, invariant and Forge gates. A measured win is never
  presented as promotion authority. Each comparison offers a downloadable
  Markdown evidence summary containing identifiers and operator-visible
  aggregates; it contains no task payloads, sealed answers or credentials and
  is not itself a signed receipt. Direct runs and authority/denial history are
  shown alongside the comparisons.

## Security model

This mirrors and reuses the TUI's daemon client
(`apps/hephaestus-tui/src/client.ts`) and wire protocol
(`apps/hephaestus-tui/src/protocol.ts`) directly rather than re-implementing
token handling: the same owner-only 0700 data directory check, `O_NOFOLLOW`
0600 `operator.token` read, and single-JSON-request-per-connection framing
over `control.sock` apply here exactly as they do in the TUI. See
[docs/CONTROL_PLANE.md](../../docs/CONTROL_PLANE.md) and
[docs/THREAT_MODEL.md](../../docs/THREAT_MODEL.md).

On top of that, this HTTP surface adds:

- **Loopback-only bind**: the server listens on `127.0.0.1` alone.
- **Read-only allowlist** (`src/allowlist.ts`): only `status`, `world_list`,
  `genome_list`, `genome_show`, `genome_prompt`, `champion_show`,
  `job_status`, `gene_list`, `gene_show`, `drift_show`, `drift_list`,
  `canary_show`, `canary_list`, `meta_strategy_show`, `meta_strategy_list`,
  `meta_show`, `meta_list`, `run_list`, `evaluation_list`, and `denial_list`
  can ever reach the daemon. A request naming any other
  command — including every mutating command the daemon protocol defines
  (`freeze`, `champion_rollback`, `daemon_stop`, ...) — is refused with
  `400` before the daemon socket is touched.
- **Per-launch session token**: a fresh random token is required as the
  `x-hephaestus-web-token` header on every `/api/command` call, compared in
  constant time. A closed console and a freshly started one never share a
  token.
- **DNS-rebinding defense**: every request's `Host` header must name this
  server's exact loopback hostname and port (`src/security.ts`); a request
  whose `Host` names anything else — including a hostname that itself
  resolves to `127.0.0.1` — is refused with `421`, so a page that only
  *looks* like it is talking to this console cannot rebind past the
  browser's same-origin policy.
- **CSRF defense**: a browser-sent `Origin` header must also match this
  server; cross-origin `fetch`/`XHR` calls are refused with `403`. No
  response ever sets an `Access-Control-Allow-Origin` header, so a browser
  refuses a cross-origin request before it is even sent.
- **Strict CSP**: `default-src 'self'` with no inline script or style and no
  remote origins anywhere; nothing is loaded from a CDN. The bundled
  `dist/web/app.js` and `dist/web/styles.css` are the entire client.
- **Bounded request bodies**: `/api/command` rejects a body over 8&nbsp;KiB
  (`413`) before it is parsed.

## Development checks

```sh
npm ci
npm run typecheck
npm test
npm run build
```

`npm run build` bundles the server (`dist/main.mjs`, an esbuild bundle like
the TUI's `build:package`) and the browser console (`dist/web/app.js`,
`dist/web/index.html`, `dist/web/styles.css`) with no external network
access at build or runtime.

`npm test` covers: the allowlist refusing every command outside its fixed
list (including every command the protocol currently defines that is not on
it); session-token and `Host`/`Origin` checks; and an end-to-end run against
a fake Unix-socket daemon (the same pattern as
`apps/hephaestus-tui/test/protocol.test.tsx`) proving an allowed command is
proxied and a disallowed one never reaches the socket.

## Not in this slice

The browser cannot seed/promote/rollback a Champion, register a Genome or
World, submit or kill a job. It does not display sealed task-level evidence or
certify a current Champion from a historical evaluation. Use the CLI/TUI for
operator actions and `hephaestus replay` to verify canonical history.
See [product readiness](../../docs/PRODUCT_READINESS.md) for remaining gaps.
