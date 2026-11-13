# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# The GPU drivers a dev shell can run the app with. `.gpu_driver` in this folder, which git
# ignores, chooses one, and .envrc enters the shell of that name: `nvidia` for NVIDIA's driver,
# `mesa` for Mesa's drivers (Intel and AMD), or the default shell, for none, where the file is
# missing or empty.
#
# Each shell has `nixVulkan`, which runs a command with the driver for Vulkan; in the default shell
# it says how to choose a driver.
{
  pkgs,
  nixpkgs,
  nixgl,
}: let
  system = pkgs.stdenv.hostPlatform.system;
  # nixGL reads the version of the host's NVIDIA driver from /proc/driver/nvidia/version, which
  # needs `--impure`, and builds the libraries of the same version, which are unfree.
  nixGL =
    import (pkgs.applyPatches {
      name = "nixgl-patched";
      src = nixgl;
      patches = [
        ./patches/nixgl-nvidia-595.patch
        ./patches/nixgl-vulkan-ld-warning.patch
      ];
    }) {
      pkgs = import nixpkgs {
        inherit system;
        config.allowUnfree = true;
      };
      # Nothing here is 32-bit, and with 32-bit drivers nixVulkan lists the i686 Mesa drivers
      # beside the x86_64 ones, so the Vulkan loader loads each twice and lists every GPU twice.
      enable32bits = false;
      enableIntelX86Extensions = system == "x86_64-linux";
    };

  # Mesa's device selection layer, which nixVulkanIntel does not list.
  mesaVulkanEnv = pkgs.writeShellScript "mesa-vulkan-env" ''
    export VK_IMPLICIT_LAYER_PATH=${pkgs.mesa}/share/vulkan/implicit_layer.d
    exec "$@"
  '';

  # `nixVulkan`, which runs a command with nixGL's script `run`, through `env` where it is not "":
  # `env` runs after `run`, so what it exports wins over nixGL's. nixGL adds the Vulkan driver
  # list it starts with to its own, so the list is cleared first.
  wrapper = run: env:
    pkgs.writeShellScriptBin "nixVulkan" ''
      unset VK_ICD_FILENAMES VK_DRIVER_FILES MESA_VK_DEVICE_SELECT
      export LD_LIBRARY_PATH="${pkgs.vulkan-loader}/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      exec ${run}/bin/${run.name} ${env} "$@"
    '';

  missing = pkgs.writeShellScriptBin "nixVulkan" ''
    echo "$0: write nvidia or mesa to .gpu_driver in examples/gpui to choose a GPU driver, then run direnv reload" >&2
    exit 1
  '';
in {
  default = missing;
  nvidia = wrapper nixGL.auto.nixVulkanNvidia "";
  mesa = wrapper nixGL.nixVulkanIntel mesaVulkanEnv;
}
