# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An image viewer for a NAS-hosted photo library, browsed over HTTP. Folders are
modelled as a graph of museum rooms; each room is one wall hung in tiers and
scrolled sideways, with doors at its left and right ends leading to adjacent
rooms. Clicking a picture opens the original over the top of it.

Three documents, each with one job — keep them that way rather than letting them
overlap:

- **`.local/status.md`** — where the work stands, what is undecided, what to do
  next, and the measurements already taken. Gitignored and kept on this machine
  only. **Read this first when picking the project back up**, and record
  progress there rather than in this file.
- **`docs/design.md`** — the authority on the design and the staged plan, and on
  which decisions were deliberate. Read before changing behaviour.
- **`CLAUDE.md`** (this file) — how to build and run it, and the invariants that
  must survive edits.

The NAS is a self-hosted NixOS machine and lumier runs on it, reading the
library from local disk. An earlier plan read it over a CIFS mount from a
neighbouring machine; passages below that worry about CIFS round trips date
from that plan.

## Commands

The Rust toolchain (cargo, rustc, clippy, rustfmt, rust-analyzer) comes from
the NixOS system configuration, not rustup, so there is no `rust-toolchain.toml`.
Edition 2024 needs Rust 1.85 or later.

```sh
# Rust
cargo build
cargo test                      # all crates
cargo test -p museum-core       # one crate
cargo test -p museum-core museum::tests::hangs_a_reciprocal_door_at_the_opposite_end
cargo clippy --all-targets

# Frontend (in web/)
npm run dev                     # Vite dev server on :5173, proxies /api and /img to :8080
npm run build                   # tsc -b && vite build -> web/dist
npm run lint                    # oxlint

# Development fixtures: a mix of aspect ratios, index marked by blocks.
# One folder per room in museum.sample.yaml.
for d in hall north south east; do node tools/make-sample-images.mjs sample-photos/$d 12; done
```

### Running it

`sample-photos/` holds the generated fixtures (gitignored), with
`museum.sample.yaml` as its manifest: a hub whose right end branches. The
manifest is kept outside the library and passed with `--manifest`, which is the
shape this wants on a NAS: the graph is version-controlled in the repo, the
share stays untouched.

The real screenshot library `ss/` and its `museum.ss.yaml`, which the
measurements below were taken on, were not carried over when the project moved
to this machine.

`tools/dev.sh` is the one-command way to check the app in a browser: it
generates the fixtures if missing, starts the API server, waits for it to
listen, then runs Vite on :5173; Ctrl-C stops both. `LUMIER_ROOT` and
`LUMIER_MANIFEST` point it at another library. `--lan` also serves Vite
on the LAN for checking from a phone; only 5173 is exposed, lumier stays on
127.0.0.1 behind Vite's proxy, and the NixOS firewall has to let 5173 in. Claude Code's Bash sandbox gives
each command a private network, so a server started there is unreachable from
the browser; run the script from your own terminal.

```sh
bash tools/dev.sh               # then browse http://localhost:5173

# Validate the manifest; exits 1 on errors
cargo run --bin lumier -- check --root sample-photos --manifest "$PWD/museum.sample.yaml"

# Two processes during development. The Vite proxy keeps everything
# same-origin, so the server needs no CORS handling.
cargo run --bin lumier -- serve --root sample-photos --manifest "$PWD/museum.sample.yaml"   # then browse :5173
cd web && npm run dev

# Single process against a built frontend
npm run build --prefix web
cargo run --bin lumier -- serve --root sample-photos --manifest "$PWD/museum.sample.yaml" --web-dir web/dist
```

`--manifest` is relative to `--root` unless absolute. `--cache-dir` holds
rendered thumbnails and is safe to delete; it refills on demand.

`RUST_LOG` overrides the default `info,tower_http=debug` filter. **Log targets
are `lumier::*`, not `museum_server::*`** — the `[[bin]]` name becomes the
crate root name, so thumbnail tracing is `RUST_LOG=info,lumier::thumbs=debug`.

## Architecture

```
crates/museum-core     Domain: image identity, contained path resolution, library
                       scan, manifest parsing, room-graph validation. No HTTP, no UI.
crates/museum-server   axum: HTTP API, image serving, SPA hosting, the `lumier` binary.
web/                   Vite + React + TypeScript. The wall, the doors, the router.
```

### Invariants worth preserving

**The HTTP API never accepts a filesystem path.** Images are addressed by
`ImageId` — `blake3` of the root-relative path, 16 bytes, hex. `Library` holds
the `id -> relative path` index built during the scan. Because no URL carries a
path segment, path traversal is not an input-validation problem here; it is not
expressible. Keep it that way: do not add an endpoint that takes a path. This
also means awkward filenames cost nothing — the library contains folders like
`ubi/Assassin's Creed® Valhalla` and no URL-encoding question ever arises.

**`museum_core::path::resolve` is the only route to the filesystem.** It refuses
absolute paths, refuses `.`/`..` segments, and — the actual guarantee —
canonicalizes before checking containment, which is what catches a symlink
inside the root that points outside it. Call it even for paths that came from
our own index (`Library::image_path` does), since the file may have been
replaced since the scan.

**Doors are an edge list, not a per-room field.** `museum.yaml` declares
`doors: [{from, to, side, ...}]`. Written per-room, `A` could claim a connection
that `B` does not reciprocate; as edges that inconsistency cannot be expressed.
`reciprocal: true` (the default) hangs the return door at the *opposite* end of
the destination's wall, which is what makes walking back and forth preserve
direction.

**`serve` and `check` share `load()`**, so the server can never run against a
manifest `check` would reject. `museum::build` reports every error at once
rather than the first, so one `check` run is enough to fix the file. Errors
refuse to build a graph; warnings (unreachable rooms, folders nobody hangs,
two rooms on one folder) describe a graph that works but probably is not
intended.

**`rel_path` is `#[serde(skip)]`** on both `ImageEntry` and `museum::Room`. It
must stay that way — it is the field that would leak library paths to the client.

### Why these dependencies

`tower-http`'s `ServeFile` is served via `oneshot` from the image handler rather
than mounted as a route, which is why `GET /img/{id}/orig` already answers Range
requests with 206, honours `If-None-Match` with 304, and sets `ETag`,
`Last-Modified` and `Accept-Ranges`. That behaviour is the reason the project is
on axum/tower-http; do not hand-roll file serving alongside it.

YAML comes from `serde_norway`, a maintained fork — upstream `serde_yaml` was
archived in 2024.

The frontend renders real DOM, so `loading="lazy"`, `decoding="async"` and
(from stage 3) `srcset` let the browser handle deferral and resolution
selection with its native decoders. `docs/design.md` §1 records why a canvas
toolkit was rejected for this.

Routing is ~60 hand-written lines in `web/src/route.ts` over `useSyncExternalStore`
rather than a router dependency: the entire need is one path parameter plus an
`arriveAt` flag carried in history state.

### Current API

```
GET /api/museum             Floor plan: entrance, every room, image counts, cover
                            picture, and doors.
GET /api/rooms/{room_id}    Room contents: images and the doors at each end.
GET /img/{image_id}/{size}  Image bytes. `size` is 320|640|1280|2560 (WebP, long
                            edge) or `orig` (the source file, for the lightbox).
                            `?v=` from the room API marks the response immutable.
GET /*                      Falls back to index.html when --web-dir is set.
```

A door carries `peek`, the first image of its destination. The frontend does
not render it, deliberately: a door is drawn as plain text, the room name and an
arrow, with no hover motion, so the pictures stay the only thing to look at
(`docs/design.md` §2). Bringing the glimpse back is a frontend-only change.

An image carries `w`, `h` and `v`. **`w`/`h` are the displayed dimensions, with
EXIF orientation already applied** — browsers rotate a quarter-turned photo when
rendering, so reporting the stored dimensions would lay the wall out to the
wrong shape. The frontend puts them on the `<img>` width/height attributes,
which is what gives the browser an intrinsic aspect ratio and keeps the wall
from reflowing as pictures load.

An entry also carries `kind` (`image` | `video`), and a video `duration` in
seconds. Videos (MP4/MOV) are probed by `museum_core::video`, which walks the
ISO BMFF boxes for the `tkhd` size and matrix rather than calling `ffprobe`;
a quarter-turn matrix swaps `w`/`h` just as EXIF orientation does. **A video
has no thumbnail**: `/img/{id}/{size}` answers 404 for it, the wall draws its
opening frame with `<video preload="metadata">` from `orig`, and covers and
door peeks pick the first still. A room hangs the library root with
`path: ""`. `docs/design.md` §6.6 records why this departs from the ffmpeg plan.

**Only a matching `?v=` earns `Cache-Control: immutable`.** `v` is a short hash
of (mtime, length). An absent or stale `v` falls back to ServeFile's ETag, which
revalidates — marking the bare id immutable would serve a replaced file from
cache forever.

### Thumbnails

`museum_core::thumb::render` is synchronous and self-contained: decode,
straighten by EXIF, resize with `fast_image_resize`, encode WebP through
`libwebp` (the `image` crate's WebP encoder is lossless-only, hence the extra
dependency). `museum_server::thumbs::Thumbnailer` wraps it with the three
guards a server needs, and **all three are load-bearing**:

- **Disk cache** at `{cache}/{size}/{id[0:2]}/{id}-{v}.webp`. The version is in
  the file name, not just the URL — keyed on the id alone, a replaced source
  would be served from the stale entry forever. The id's first two characters
  shard the directory.
- **Single flight**, one gate per cache path. Measured: 50 simultaneous requests
  for an uncached thumbnail cause exactly one render.
- **A semaphore sized to the core count**, held across the `spawn_blocking`
  render. This is the one that looks optional and is not: without it a few
  hundred concurrent requests fill the blocking pool and the server stops
  answering everything, JSON included. Measured: while 149 images render,
  `/api/museum` stays at a 2 ms median.

Writes go to a `.part` file beside the target and are renamed into place, so no
reader can open a half-written thumbnail.

The cache entry is keyed on the version of the file **currently on disk**, never
on the request's `?v=` — a stale query must not label a freshly rendered
thumbnail.

Sizes are a fixed set, not an arbitrary `?w=`: open-ended sizes scatter the
cache without bound. **Which one to request is chosen in JS, not with
`srcset`/`sizes`** — `sizes` can only describe a width, and this layout is
height-driven, so the drawn width follows from `--tier-height` and the image's
aspect ratio. `--tier-height` in CSS is the single source of truth and
`useTierHeight` measures it with a hidden probe element, because
`getComputedStyle` returns a custom property's unevaluated token stream rather
than a pixel value.

### The tiered wall

Pictures fill a column top to bottom and then carry on rightwards, so **a
column is never taller than the wall and the only scrolling stays sideways** —
which is what keeps the doors, the position memory and the keyboard working
unchanged. The wall is a flex row of `[left doors][tier grid][right doors]`,
with the tier grid separate so door placement does not depend on how pictures
flow into it.

Only `--tiers` varies with the viewport; `--tier-height` falls out of it. **The
tier height is capped by `40vw` as well as by height**, and that cap is not
cosmetic: a 16:9 picture is 1.78× as wide as it is tall, so a height-derived
tier on a 390px-wide phone makes every picture 405px wide and nothing fits at
all. Narrow screens then have height going spare, so a `max-width: 600px` query
hangs more tiers; it comes last so it wins over the height queries.

Column width is `max-content` — the widest picture in that column. With uniform
aspect ratios the columns pack tightly (measured 98% fill on `ss/myst`); with
mixed ones they do not (59% on the deliberately-varied fixtures, where a
portrait shares a column with a panorama). Fine for this library, so it is left
alone; a mixed photo library would want per-column packing instead.

### Where the visitor lands

Four ways into a room, and they do not all want the same answer:

| Arrived by | Lands at |
|---|---|
| a door on the right | the next wall's start |
| a door on the left | the next wall's end |
| a jump from the floor plan | the remembered position, else the start |
| back / forward / reload | the remembered position, else the start |

A door puts you at that end of the wall because **that is where the door is**;
in practice you had to walk to that end to use it, so this usually agrees with
the remembered position anyway. `route.ts` keeps the arrival mode in a module
variable rather than in history state, deliberately: going *back* to a room must
restore the position there, not replay the door originally used to reach it.

Positions live in `sessionStorage` via `scrollMemory.ts`, every access guarded —
blocked site data must never fail a render over a convenience.

### The floor plan

`FloorPlan.tsx` groups rooms by breadth-first distance from the entrance and
scrolls sideways, one column per distance. This shows the museum's shape without
a graph-layout algorithm: a corridor reads as a long row of single-room columns,
a hub as one tall column — and the gesture matches the rest of the app.

It is **a way to move, not an orientation aid**: doors alone make a deep graph
impractical, which `ss/` demonstrates — `ac_valhalla` sits 21 doors from the
entrance. Rooms unreachable through doors still get a column, because a jump is
the only way to reach them.

### The scan

`probe::probe` reads headers only — measured at 0.5–1.2 ms per image on local
SSD, with 240 MiB of PNGs probed in 27 ms, so it is definitively not decoding
pixels. One `File::open` serves both the EXIF read and the dimension read,
because opening twice would double the round trips on a network mount.

**That timing will not survive CIFS.** Each image costs a `canonicalize`, an
open, a header read and a stat — four round trips minimum. See `docs/design.md`
§6.2 for what stage 7 does about it (persistence, parallelism, and revisiting
the per-file `canonicalize`).

Ordering is natural and case-insensitive via `natord::compare_ignore_case`, so
`img2` precedes `img10`. This is load-bearing for real libraries: the Valhalla
screenshots in `ss/` are named `...2022-8-3-...` and `...2022-8-11-...`, which
lexicographic sorting puts in the wrong chronological order.

### Known-deliberate rough edges

The cache has no eviction. It grows until deleted; an LRU is listed under
§8 of the design doc.

`museum.ss.yaml` is a single 21-deep corridor, which is a deliberately plain
starting point and a poor curation — the design doc's own advice is not to put
rooms that far apart. Reordering the `doors` list is all it takes to reshape.

`order:` is deliberately *not* in the manifest schema yet, and
`deny_unknown_fields` will reject it: a field parsed but not honoured is worse
than one refused.
