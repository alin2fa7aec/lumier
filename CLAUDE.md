# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

An image viewer for a NAS-hosted photo library, browsed over HTTP. Folders are
modelled as a graph of museum rooms; each room is one wall hung in tiers and
scrolled sideways, with doors at its left and right ends leading to adjacent
rooms. Clicking a picture opens it large over the top of the wall.

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

The measurements below that mention `ss/` were taken on a private screenshot
library with its own `museum.ss.yaml`; neither is in the repository.

`tools/dev.sh` is the one-command way to check the app in a browser: it
generates the fixtures if missing, starts the API server, waits for it to
listen, then runs Vite on :5173; Ctrl-C stops both. `LUMIER_ROOT` and
`LUMIER_MANIFEST` point it at another library, and `LUMIER_PORT` moves lumier
off 8080 when an installed service already holds it. `--lan` also serves Vite
on the LAN for checking from a phone; only 5173 is exposed, lumier stays on
127.0.0.1 behind Vite's proxy, and the NixOS firewall has to let 5173 in. Claude Code's Bash sandbox gives
each command a private network, so a server started there is unreachable from
the browser; run the script from your own terminal.

```sh
bash tools/dev.sh               # then browse http://localhost:5173

# Validate the manifest; exits 1 on errors
cargo run --bin lumier -- check --root sample-photos --manifest "$PWD/museum.sample.yaml"

# Deletion marks: serve records them, purge acts on them. The trash must sit
# on the library's filesystem, outside it.
cargo run --bin lumier -- serve --root sample-photos --manifest "$PWD/museum.sample.yaml" \
  --state-dir .local/dev-state --trash-dir .local/dev-trash
cargo run --bin lumier -- purge --root sample-photos --trash-dir .local/dev-trash --state-dir .local/dev-state

# Two processes during development. The Vite proxy keeps everything
# same-origin, so the server needs no CORS handling.
cargo run --bin lumier -- serve --root sample-photos --manifest "$PWD/museum.sample.yaml"   # then browse :5173
cd web && npm run dev

# Single process against a built frontend
npm run build --prefix web
cargo run --bin lumier -- serve --root sample-photos --manifest "$PWD/museum.sample.yaml" --web-dir web/dist

# Nix: the package (binary plus built frontend) and the NixOS module.
# A flake sees only files git tracks, and jj adds a new file to the git index
# only when it next snapshots, so run a jj command first after adding files.
jj st && nix build .#lumier     # result/bin/lumier, result/share/lumier/web
```

The NAS imports `nix/module.nix` (also `nixosModules.default` in the flake) and
sets `services.lumier.{enable,root,manifest,openFirewall}`. The unit sees the
whole filesystem read-only apart from `/var/cache/lumier`, so the library
cannot be written even by a compromised server. `systemctl reload lumier`
rescans.

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

**A rescan goes through the same `load()`**, and swaps in a whole new
`Snapshot` (library index, room graph, images, versions) rather than editing
the old one. Each request clones the `Arc<Snapshot>` once and uses only that,
so it never mixes two scans. A load that fails (a half-edited manifest, say)
leaves the previous snapshot serving. The `Thumbnailer` lives outside the
snapshot, because its single-flight gates and semaphore must survive a swap.
Rescans come from `POST /api/rescan`, SIGHUP, and `--rescan-interval`, and
one lock keeps them from overlapping.

**`rel_path` is `#[serde(skip)]`** on both `ImageEntry` and `museum::Room`. It
must stay that way — it is the field that would leak library paths to the client.

**`serve` never writes to the library.** With `--state-dir` and `--trash-dir`
it takes deletion marks and restore requests, but writes them only to the
state directory. `lumier purge` (`museum_core::purge`) is the only code that
moves or deletes library files, run nightly in its own unit. Both lock the
state directory (`museum_core::state`) for every read-modify-write and the
purge holds the lock for its whole run, so a mark made mid-purge waits rather
than being lost. A mark is the path plus the version at marking time; the
purge skips a file whose version has changed, so a file replaced under the
same name is never the one that goes. Files reach the trash at
`{day}/{library path}` and come back by the reverse rename, which
`path::resolve_new` makes containment-checked like every other path. The
trash is scanned as a `Library` whose ids hash a `trash:`-prefixed path, so
trash ids cannot collide with library ids and `/img` serves both.

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
GET /api/notes/{image_id}   A note as sanitized HTML, links pointing at ids.
POST /api/rescan            Re-read the manifest and rescan the library. 409
                            while one runs, 422 (details in the log only) if
                            the load fails.
GET /img/{image_id}/{size}  Image bytes. `size` is 320|640|1280|2560 (WebP, long
                            edge) or `orig` (the source file: videos, GIFs, and
                            "open the original" from the lightbox).
                            `?v=` from the room API marks the response immutable.
PUT|DELETE /api/marks/{image_id}
                            Mark for deletion, or take the mark back. Only with
                            --state-dir/--trash-dir; PUT and DELETE from another
                            Origin get 403.
GET /api/marks              Every marked picture, room by room, as a door-less
                            room the client calls `~marks`.
GET /api/trash              The trash, newest day first, with `day`,
                            `days_left` and `restore`, as `~trash`.
PUT|DELETE /api/trash/{image_id}/restore
                            Ask the next purge to put a file back, or not.
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

An entry also carries `kind` (`image` | `video` | `pdf` | `markdown`), and a video `duration` in
seconds. Videos (MP4/MOV) are probed by `museum_core::video`, which walks the
ISO BMFF boxes for the `tkhd` size and matrix rather than calling `ffprobe`;
a quarter-turn matrix swaps `w`/`h` just as EXIF orientation does. **A video
has no thumbnail**: `/img/{id}/{size}` answers 404 for it, the wall draws its
opening frame with `<video preload="metadata">` from `orig`, and covers and
door peeks pick the first still. A room hangs the library root with
`path: ""`. `docs/design.md` §6.6 records why this departs from the ffmpeg plan.

**A PDF hangs as its first page.** `museum_core::pdf` reads the page size
(points, rotation and crop box applied) and draws the page with `hayro`, a
PDF rasterizer in Rust, so no C library is needed at run time — the same
reasoning as the video probe. A PDF must be parsed whole, so the sizes found
are remembered per path and version and a rescan rereads only changed files.
The thumbnail path draws the page straight at the requested size instead of
decoding and scaling. Parsing and drawing run under `catch_unwind`: a broken
PDF costs its own thumbnail, not the scan. The lightbox shows the 2560
rendition and opens `orig` in a new tab for the browser's own viewer. The
slideshow skips PDFs, as it does videos.

**A note hangs as a card and its links are resolved by file name.**
`museum_core::note` gives the card a title and excerpt and a nominal
300×400 shape, so it packs like a picture; like a video it has no thumbnail.
`GET /api/notes/{id}` converts it with `comrak`, rewrites every link and
embed on the syntax tree to `/img/{id}/…` through `LinkIndex` (names
NFC-normalized and case-folded; the note's own room wins, else the name must
be unique), and cleans the result with `ammonia` — the single place that
decides which HTML survives, which is why comrak may pass raw HTML through.
The browser therefore never sees a path, here either. A link that resolves
nowhere keeps its text in `span.link--broken`, and `lumier check` lists it.

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
`useTierLayout` measures it (with `--tiers` and `--tier-gap`) through a hidden
probe element, because
`getComputedStyle` returns a custom property's unevaluated token stream rather
than a pixel value.

### The tiered wall

Each tier is a horizontal band, and `pack` in `Wall.tsx` puts each picture
in turn into **the band that is shortest so far**, measured from the API's
`w`/`h` before any pixels arrive. Pictures of any shape then pack edge to
edge and only the right-hand end is ragged; the bands never outgrow the wall,
so **the only scrolling stays sideways** — which is what keeps the doors, the
position memory and the keyboard working unchanged. The price is order:
pictures run roughly, not strictly, left to right, and the lightbox still steps
through them in the room's own order. The wall is a flex row of
`[left doors][bands][right doors]`, with the bands separate so door placement
does not depend on how pictures flow into them.

Only `--tiers` varies with the viewport; `--tier-height` falls out of it. **The
tier height is capped by `40vw` as well as by height**, and that cap is not
cosmetic: a 16:9 picture is 1.78× as wide as it is tall, so a height-derived
tier on a 390px-wide phone makes every picture 405px wide and nothing fits at
all. Narrow screens then have height going spare, so a `max-width: 600px` query
hangs more tiers; it comes last so it wins over the height queries.

The bands replaced a column-major grid whose columns were as wide as their
widest picture. On the deliberately mixed fixtures that filled 59% of the wall
at four tiers, against 81% for the bands (64% against 91% at three), the rest
being the ragged right end of rooms of a dozen pictures.

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

A room's `order:` is `name` (the default), `mtime` or `taken`. By name,
ordering is natural and case-insensitive via `natord::compare_ignore_case`, so
`img2` precedes `img10`. `taken` reads EXIF `DateTimeOriginal` (with its
`OffsetTimeOriginal`, else as UTC) and a video's `mvhd` creation time, and
falls back to the mtime; the name breaks ties in both time orders. This is load-bearing for real libraries: the Valhalla
screenshots in `ss/` are named `...2022-8-3-...` and `...2022-8-11-...`, which
lexicographic sorting puts in the wrong chronological order.

### Known-deliberate rough edges

The cache has no eviction. It grows until deleted; an LRU is listed under
§8 of the design doc.

`museum.ss.yaml` is a single 21-deep corridor, which is a deliberately plain
starting point and a poor curation — the design doc's own advice is not to put
rooms that far apart. Reordering the `doors` list is all it takes to reshape.

`deny_unknown_fields` stays on every manifest struct: a field parsed but not
honoured is worse than one refused.
