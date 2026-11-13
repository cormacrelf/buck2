# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.
{
  description = "A shell to build and run the gpui example";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Runs the app with the host's GPU driver (nix/gpu.nix), patched in nix/patches/nixgl-*.patch.
    nixgl = {
      url = "github:nix-community/nixGL/b6105297e6f0cd041670c3e8628394d4ee247ed5";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs = { self, nixpkgs, flake-utils, nixgl }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system: let
      pkgs = import nixpkgs { inherit system; };
      gpu = import ./nix/gpu.nix { inherit pkgs nixpkgs nixgl; };
      # The libraries the app needs when it starts, other than Vulkan, which `nixVulkan` gives it:
      # those it links, which the binary does not say where to find, and those gpui loads.
      loaded = [
        pkgs.fontconfig.lib
        pkgs.freetype
        pkgs.libxcb
        pkgs.libxkbcommon
        pkgs.wayland
      ];
      shell = nixVulkan: pkgs.mkShell {
        # The prelude's clang toolchain links with `-fuse-ld=lld` on Linux.
        packages = [ nixVulkan pkgs.clang pkgs.lld pkgs.python3 pkgs.rustc pkgs.watchman ];
        # What gpui links. clang finds them through these.
        buildInputs = [ pkgs.freetype pkgs.libxcb pkgs.libxkbcommon ];
        shellHook = ''
          export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath loaded}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
        '';
      };
    in {
      # `default`, and `nvidia` and `mesa` for the GPU driver that .gpu_driver chooses (see .envrc).
      devShells = builtins.mapAttrs (_: shell) gpu;
    });
}
