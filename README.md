# lumier

A browser-based viewer for a photo library on a home server, laid out as a
museum. Each folder is a room: one long wall, hung in tiers and scrolled
sideways, with doors at either end leading to the next rooms. Click a picture
to see it large; the slideshow walks the whole museum.

Photos, videos (MP4/MOV), PDFs and Markdown notes all hang on the walls.
Thumbnails are rendered on demand as WebP and cached on disk. The library
itself is never written by the server.

## The manifest

Which folder is which room, and how rooms connect, is declared in a YAML file
kept outside the library:

```yaml
version: 1
entrance: hall

rooms:
  - { id: hall, name: Hall, path: hall }
  - { id: north, name: North Wing, path: north }
  - { id: south, name: South Wing, path: south }

doors:
  - { from: hall, to: north, side: right }
  - { from: hall, to: south, side: right }
```

Doors are reciprocal by default: the return door hangs at the opposite end of
the destination's wall, so walking back and forth keeps its direction.
`lumier check` validates a manifest against the library and reports every
problem at once.

## Running it

Rust 1.85 or later and Node.js are needed.

```sh
# Generate sample images and run the API server plus the Vite dev server
bash tools/dev.sh               # then browse http://localhost:5173

# Or as a single process against the built frontend
npm ci --prefix web && npm run build --prefix web
cargo run --release --bin lumier -- serve \
  --root sample-photos --manifest "$PWD/museum.sample.yaml" --web-dir web/dist
```

`lumier --help` lists the subcommands (`serve`, `check`, `purge`) and their
options.

### NixOS

The flake provides the package and a NixOS module:

```nix
{
  inputs.lumier.url = "github:alin2fa7aec/lumier";

  outputs = { nixpkgs, lumier, ... }: {
    nixosConfigurations.nas = nixpkgs.lib.nixosSystem {
      modules = [
        lumier.nixosModules.default
        {
          services.lumier = {
            enable = true;
            root = "/srv/photos";
            manifest = ./museum.yaml;
          };
        }
      ];
    };
  };
}
```

The service sees the filesystem read-only apart from its cache.
`systemctl reload lumier` rescans the library.

To serve several museums from one machine, say two collections that should
not share a floor plan or a slideshow, give each its own manifest and port
under `services.lumier.instances`. Each runs as `lumier-<name>.service` with
its own cache and state:

```nix
services.lumier.instances = {
  family = { root = "/srv/photos"; manifest = ./family.yaml; port = 8080; };
  travel = { root = "/srv/photos"; manifest = ./travel.yaml; port = 8081; };
};
```

## Security

**lumier has no authentication.** It is meant for a trusted LAN (or a VPN such
as Tailscale), not for the open internet. Anyone who can reach the port can
browse the whole library, and, if deletion marks are enabled, mark files for
the nightly purge.

The binary binds `127.0.0.1:8080` by default. The NixOS module listens on
`0.0.0.0` and leaves the firewall closed unless `openFirewall` is set.

## Documentation

- [`docs/design.md`](docs/design.md): the design, its rationale and the
  staged plan (in Japanese).
- [`CLAUDE.md`](CLAUDE.md): building, the architecture, and the invariants
  the code relies on.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
