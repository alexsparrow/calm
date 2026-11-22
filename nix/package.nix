# calm, built with Nix. `nix build`, `nix run`, or the overlay in flake.nix.
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  wayland,
  libxkbcommon,
  vulkan-loader,
  libGL,
  fontconfig,
  freetype,
  libx11,
  libxcb,
  libxcursor,
  libxrandr,
  libxi,
  sqlite,
}:

let
  cargo = lib.importTOML ../Cargo.toml;
  # gpui opens these at runtime (Vulkan, Wayland, X11) rather than linking them.
  runtimeLibs = [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    fontconfig
    freetype
    libx11
    libxcb
    libxcursor
    libxrandr
    libxi
  ];
in
rustPlatform.buildRustPackage {
  pname = cargo.package.name;
  version = cargo.package.version;

  # Just the sources: no build output, devenv state or editor files.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../assets
      ../patches
      ../tests
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  # The SQLite crate links the system library when building under Nix.
  buildInputs = runtimeLibs ++ [ sqlite ];

  # The tests need nothing outside the sandbox (fake Google, in-memory and temp-file stores).
  doCheck = true;

  postFixup = ''
    wrapProgram $out/bin/calm --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibs}
  '';

  meta = {
    description = "A calendar viewer built on a grammar of time";
    mainProgram = "calm";
    platforms = lib.platforms.linux;
  };
}
