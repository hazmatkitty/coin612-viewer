{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  env.PYGAME_HIDE_SUPPORT_PROMPT = "1";
  packages = [
    (pkgs.python3.withPackages (ps: [
      ps.pyusb
      ps.pygame
      ps.numpy
      (ps.opencv4.override { enableGtk3 = true; })
    ]))
    pkgs.libusb1
    pkgs.ffmpeg
    # Rust viewer (coin612-rs)
    pkgs.rustc
    pkgs.cargo
    pkgs.pkg-config
    pkgs.SDL2
  ];
}
