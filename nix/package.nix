{
  lib,
  stdenv,
  rustPlatform,
  buildNpmPackage,
  importNpmLock,
  autoPatchelfHook,
}:

let
  fs = lib.fileset;

  # The built frontend. importNpmLock reads package-lock.json directly, so
  # there is no npmDepsHash to keep in step with the lock file.
  web = buildNpmPackage {
    pname = "lumier-web";
    version = "0.0.0";
    src = fs.toSource {
      root = ../web;
      fileset = fs.difference ../web (
        fs.unions [
          (fs.maybeMissing ../web/node_modules)
          (fs.maybeMissing ../web/dist)
        ]
      );
    };
    npmDeps = importNpmLock { npmRoot = ../web; };
    npmConfigHook = importNpmLock.npmConfigHook;

    # Vite's bundler ships as a prebuilt native module, linked for a
    # conventional distribution rather than for the Nix store.
    nativeBuildInputs = [ autoPatchelfHook ];
    buildInputs = [ stdenv.cc.cc.lib ];
    # importNpmLock installs every optional platform package, musl builds
    # included. Node here is glibc, so those are never loaded.
    autoPatchelfIgnoreMissingDeps = [ "libc.musl-*.so.1" ];
    preBuild = ''
      autoPatchelf node_modules
    '';

    installPhase = ''
      runHook preInstall
      cp -r dist $out
      runHook postInstall
    '';
  };
in
rustPlatform.buildRustPackage {
  pname = "lumier";
  version = "0.0.0";

  src = fs.toSource {
    root = ../.;
    fileset = fs.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  postInstall = ''
    mkdir -p $out/share/lumier
    ln -s ${web} $out/share/lumier/web
  '';

  passthru = { inherit web; };

  meta = {
    description = "A museum-shaped viewer for a photo library";
    homepage = "https://github.com/alin2fa7aec/lumier";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "lumier";
    platforms = lib.platforms.linux;
  };
}
