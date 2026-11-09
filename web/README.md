# Crouton Alpha web dashboard

This directory is a **static, dependency-free frontend**. The Rust backend also
serves it directly when run in API mode (`cargo run --release -- serve`).

## Run it with the Rust backend

```bash
cargo run --release -- serve
# open http://127.0.0.1:8080
```

The API listens on `0.0.0.0:8080` by default. Override with:

```bash
PORT=3000 cargo run --release -- serve
```

## Deploy the frontend on Vercel

1. Deploy this `web` directory as a static site.
2. In the dashboard, set the API base to the Rust service (a Box host or any
   public URL you run the Rust backend at), or add `?api=https://your-api.example`
   to the homepage URL.
3. The browser request is cross-origin; the Rust API already returns permissive
   CORS headers (`Access-Control-Allow-Origin: *`).

No npm/Vite build is required: `index.html`, `app.js`, and `styles.css` are
plain static files.

## Run that backend on a Box / cloud computer

A Box (or any clean Linux host) only needs this repo and a Rust toolchain:

```bash
git clone <your fm-cli remote> fm-cli
cd fm-cli
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
bash scripts/start-box.sh
```

That leaves the Rust API+dashboard listening on port 8080 (set `PORT=3000 bash
scripts/start-box.sh` to change it).

Then, back on Vercel, set the frontend's **API endpoint** to your Box host
(e.g. `https://api.example.com`) — either in the dashboard's left panel or via
the `?api=https://api.example.com` query parameter. The Rust backend already
returns permissive CORS headers. If you'd rather keep everything on one host,
the same Rust process serves the dashboard too at the API URL.

## Player database endpoints

The player laboratory merges two different universes:

* the **live world pool** (players actually selected into the season world,
  including fully generated world-only players); and
* the **full `data/players.csv` roster** (every source player, including
  full-database players never promoted into this world's active pool).

```text
GET /api/players
GET /api/players?scope=all&sort=ovr&limit=250
GET /api/players?scope=database&sort=ovr&limit=250
GET /api/players?scope=world&sort=goals&limit=250
GET /api/player/db:{PlayerId}
GET /api/player/db/{PlayerId}
GET /api/player/wp:{world-index}
GET /api/player/world/{world-index}
```

`scope=all` defaults to a deduplicated merge: a source player that is also in
the live world appears once as the live profile, with the original contract,
personality, foot, traits and market value attached as `profile` and
`source_attrs`. `scope=database` returns every matching source row, including
source-only prospect records; `scope=world` returns the live generated and
database-backed world players. Filters include `q`/`search`, `nation`,
`league`, `position`, and `min_ovr`; sort options include `goals`, `assists`,
`rating`, `apps`, `ovr`, `future`, `created`, `kp`, `tackles`, and `name`.
