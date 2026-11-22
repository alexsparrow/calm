{ pkgs, lib, ... }:

let
  # Libraries gpui links against or dlopens at runtime on Linux.
  runtimeLibs = with pkgs; [
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    fontconfig
    freetype
    xorg.libX11
    xorg.libxcb
    xorg.libXcursor
    xorg.libXrandr
    xorg.libXi
    xorg.xcbutil
  ];
in
{
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [ "rustc" "cargo" "clippy" "rustfmt" "rust-analyzer" "rust-src" ];
  };

  packages = with pkgs; [
    pkg-config
    clang
    mold
    cmake
    openssl
    vulkan-tools
  ] ++ runtimeLibs;

  env.LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
  languages.rust.rustflags = "-C link-arg=-fuse-ld=mold";
}
