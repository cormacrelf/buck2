# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is licensed under both the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree and the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree.

# Update with
#   python3 -c 'import json, requests; print(json.dumps(requests.get("https://ziglang.org/download/index.json").json(), indent=4))'
releases = {
    "master": {
        "version": "0.12.0-dev.286+b0d9bb0bb",
        "date": "2023-09-07",
        "docs": "https://ziglang.org/documentation/master/",
        "stdDocs": "https://ziglang.org/documentation/master/std/",
        "src": {
            "tarball": "https://ziglang.org/builds/zig-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "f7c90be8f94c7a847fdacf86b3290130ef9b97c8dea13634b211e042face6cc4",
            "size": "15306060"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/builds/zig-bootstrap-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "6038b0e1bb5a4439b5c91f79e3040cc69c86e39d612cc48ac24dee04ee56563b",
            "size": "43267776"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/builds/zig-macos-x86_64-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "b6049953c0ac5b8dd6f7bf1fb7dcaecc5339c6327ba528dd39c8b8c7748248b3",
            "size": "47210224"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/builds/zig-macos-aarch64-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "96d818034cdc4d57e5dd75c522c465f41819bb12e75bb13009050ffc3dfef0ad",
            "size": "43884740"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-x86_64-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "90c024654fb6e17342782e39071de1074b4afbdc9115ac401a6e5405e3131635",
            "size": "44998592"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-aarch64-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "9ca14995656927603b7fbc6af8ce89543982cd0b127969db414268e6d769b250",
            "size": "41523280"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-armv7a-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "46b61d675beae75ef4692e16e52a29fcdd103d20d286a7a9eb196cf498064b09",
            "size": "42261484"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-riscv64-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "d47b7dcd12284f43be8334e8020a05f46c013598cf691c625a89c8ae09e36336",
            "size": "43538924"
        },
        "powerpc64le-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-powerpc64le-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "48eff5e27ff3ea5e12817e084393d3fd15a906dd0487b652ef01d60cd0484651",
            "size": "44690416"
        },
        "powerpc-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-powerpc-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "0acde9ee13f19b9c3b1d8ff118a56ea032c0d8a1b41b065fd8f31cf24ef3a2c5",
            "size": "44570944"
        },
        "x86-linux": {
            "tarball": "https://ziglang.org/builds/zig-linux-x86-0.12.0-dev.286+b0d9bb0bb.tar.xz",
            "shasum": "31608bcf8f8ec4eee3130daf7bfad07a88ef4b2c9a60838db14db5ec91cdfdd3",
            "size": "49858284"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/builds/zig-windows-x86_64-0.12.0-dev.286+b0d9bb0bb.zip",
            "shasum": "ab75c905e12f721b3be1c764b906aad5e2c121561f0c4d7804e2e6197a14af42",
            "size": "77765413"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/builds/zig-windows-aarch64-0.12.0-dev.286+b0d9bb0bb.zip",
            "shasum": "c1289e1cafe97831cc127e31b48765b2dd8a8279ea0aa255e53d08677444a9fb",
            "size": "74446939"
        },
        "x86-windows": {
            "tarball": "https://ziglang.org/builds/zig-windows-x86-0.12.0-dev.286+b0d9bb0bb.zip",
            "shasum": "66dc6dd5866c1ced53de208a47abc2c5156cc8c50ff3b1adce6b022a046dfc86",
            "size": "82115784"
        }
    },
    "0.11.0": {
        "date": "2023-08-04",
        "docs": "https://ziglang.org/documentation/0.11.0/",
        "stdDocs": "https://ziglang.org/documentation/0.11.0/std/",
        "notes": "https://ziglang.org/download/0.11.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-0.11.0.tar.xz",
            "shasum": "72014e700e50c0d3528cef3adf80b76b26ab27730133e8202716a187a799e951",
            "size": "15275316"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-bootstrap-0.11.0.tar.xz",
            "shasum": "38dd9e17433c7ce5687c48fa0a757462cbfcbe75d9d5087d14ebbe00efd21fdc",
            "size": "43227592"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-macos-x86_64-0.11.0.tar.xz",
            "shasum": "1c1c6b9a906b42baae73656e24e108fd8444bb50b6e8fd03e9e7a3f8b5f05686",
            "size": "47189164"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-macos-aarch64-0.11.0.tar.xz",
            "shasum": "c6ebf927bb13a707d74267474a9f553274e64906fd21bf1c75a20bde8cadf7b2",
            "size": "43855096"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-x86_64-0.11.0.tar.xz",
            "shasum": "2d00e789fec4f71790a6e7bf83ff91d564943c5ee843c5fd966efc474b423047",
            "size": "44961892"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-aarch64-0.11.0.tar.xz",
            "shasum": "956eb095d8ba44ac6ebd27f7c9956e47d92937c103bf754745d0a39cdaa5d4c6",
            "size": "41492432"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-armv7a-0.11.0.tar.xz",
            "shasum": "aebe8bbeca39f13f9b7304465f9aee01ab005d243836bd40f4ec808093dccc9b",
            "size": "42240664"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-riscv64-0.11.0.tar.xz",
            "shasum": "24a478937eddb507e96d60bd4da00de9092b3f0920190eb45c4c99c946b00ed5",
            "size": "43532324"
        },
        "powerpc64le-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-powerpc64le-0.11.0.tar.xz",
            "shasum": "75260e87325e820a278cf9e74f130c7b3d84c0b5197afb2e3c85eff3fcedd48d",
            "size": "44656184"
        },
        "powerpc-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-powerpc-0.11.0.tar.xz",
            "shasum": "70a5f9668a66fb2a91a7c3488b15bcb568e1f9f44b95cd10075c138ad8c42864",
            "size": "44539972"
        },
        "x86-linux": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-linux-x86-0.11.0.tar.xz",
            "shasum": "7b0dc3e0e070ae0e0d2240b1892af6a1f9faac3516cae24e57f7a0e7b04662a8",
            "size": "49824456"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-windows-x86_64-0.11.0.zip",
            "shasum": "142caa3b804d86b4752556c9b6b039b7517a08afa3af842645c7e2dcd125f652",
            "size": "77216743"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-windows-aarch64-0.11.0.zip",
            "shasum": "5d4bd13db5ecb0ddc749231e00f125c1d31087d708e9ff9b45c4f4e13e48c661",
            "size": "73883137"
        },
        "x86-windows": {
            "tarball": "https://ziglang.org/download/0.11.0/zig-windows-x86-0.11.0.zip",
            "shasum": "e72b362897f28c671633e650aa05289f2e62b154efcca977094456c8dac3aefa",
            "size": "81576961"
        }
    },
    "0.10.1": {
        "date": "2023-01-19",
        "docs": "https://ziglang.org/documentation/0.10.1/",
        "stdDocs": "https://ziglang.org/documentation/0.10.1/std/",
        "notes": "https://ziglang.org/download/0.10.1/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-0.10.1.tar.xz",
            "shasum": "69459bc804333df077d441ef052ffa143d53012b655a51f04cfef1414c04168c",
            "size": "15143112"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-bootstrap-0.10.1.tar.xz",
            "shasum": "9f5781210b9be8f832553d160851635780f9bd71816065351ab29cfd8968f5e9",
            "size": "43971816"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-macos-x86_64-0.10.1.tar.xz",
            "shasum": "02483550b89d2a3070c2ed003357fd6e6a3059707b8ee3fbc0c67f83ca898437",
            "size": "45119596"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-macos-aarch64-0.10.1.tar.xz",
            "shasum": "b9b00477ec5fa1f1b89f35a7d2a58688e019910ab80a65eac2a7417162737656",
            "size": "40517896"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-linux-x86_64-0.10.1.tar.xz",
            "shasum": "6699f0e7293081b42428f32c9d9c983854094bd15fee5489f12c4cf4518cc380",
            "size": "44085596"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-linux-aarch64-0.10.1.tar.xz",
            "shasum": "db0761664f5f22aa5bbd7442a1617dd696c076d5717ddefcc9d8b95278f71f5d",
            "size": "40321280"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-linux-riscv64-0.10.1.tar.xz",
            "shasum": "9db5b59a5112b8beb995094ba800e88b0060e9cf7cfadf4dc3e666c9010dc77b",
            "size": "42196008"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-linux-i386-0.10.1.tar.xz",
            "shasum": "8c710ca5966b127b0ee3efba7310601ee57aab3dd6052a082ebc446c5efb2316",
            "size": "48367388"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-windows-x86_64-0.10.1.zip",
            "shasum": "5768004e5e274c7969c3892e891596e51c5df2b422d798865471e05049988125",
            "size": "73259729"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/download/0.10.1/zig-windows-aarch64-0.10.1.zip",
            "shasum": "ece93b0d77b2ab03c40db99ef7ccbc63e0b6bd658af12b97898960f621305428",
            "size": "69417459"
        }
    },
    "0.10.0": {
        "date": "2022-10-31",
        "docs": "https://ziglang.org/documentation/0.10.0/",
        "stdDocs": "https://ziglang.org/documentation/0.10.0/std/",
        "notes": "https://ziglang.org/download/0.10.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-0.10.0.tar.xz",
            "shasum": "d8409f7aafc624770dcd050c8fa7e62578be8e6a10956bca3c86e8531c64c136",
            "size": "14530912"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-bootstrap-0.10.0.tar.xz",
            "shasum": "c13dc70c4ff4c09f749adc0d473cbd3942991dd4d1bd2d860fbf257d8c1bbabf",
            "size": "45625516"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-freebsd-x86_64-0.10.0.tar.xz",
            "shasum": "dd77afa2a8676afbf39f7d6068eda81b0723afd728642adaac43cb2106253d65",
            "size": "44056504"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-linux-aarch64-0.10.0.tar.xz",
            "shasum": "09ef50c8be73380799804169197820ee78760723b0430fa823f56ed42b06ea0f",
            "size": "40387688"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-linux-armv7a-0.10.0.tar.xz",
            "shasum": "7201b2e89cd7cc2dde95d39485fd7d5641ba67dc6a9a58c036cb4c308d2e82de",
            "size": "50805936"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-linux-i386-0.10.0.tar.xz",
            "shasum": "dac8134f1328c50269f3e50b334298ec7916cb3b0ef76927703ddd1c96fd0115",
            "size": "48451732"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-linux-riscv64-0.10.0.tar.xz",
            "shasum": "2a126f3401a7a7efc4b454f0a85c133db1af5a9dfee117f172213b7cbd47bfba",
            "size": "42272968"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-linux-x86_64-0.10.0.tar.xz",
            "shasum": "631ec7bcb649cd6795abe40df044d2473b59b44e10be689c15632a0458ddea55",
            "size": "44142400"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-macos-aarch64-0.10.0.tar.xz",
            "shasum": "02f7a7839b6a1e127eeae22ea72c87603fb7298c58bc35822a951479d53c7557",
            "size": "40602664"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-macos-x86_64-0.10.0.tar.xz",
            "shasum": "3a22cb6c4749884156a94ea9b60f3a28cf4e098a69f08c18fbca81c733ebfeda",
            "size": "45175104"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-windows-x86_64-0.10.0.zip",
            "shasum": "a66e2ff555c6e48781de1bcb0662ef28ee4b88af3af2a577f7b1950e430897ee",
            "size": "73181558"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/download/0.10.0/zig-windows-aarch64-0.10.0.zip",
            "shasum": "1bbda8d123d44f3ae4fa90d0da04b1e9093c3f9ddae3429a4abece1e1c0bf19a",
            "size": "69332389"
        }
    },
    "0.9.1": {
        "date": "2022-02-14",
        "docs": "https://ziglang.org/documentation/0.9.1/",
        "stdDocs": "https://ziglang.org/documentation/0.9.1/std/",
        "notes": "https://ziglang.org/download/0.9.1/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-0.9.1.tar.xz",
            "shasum": "38cf4e84481f5facc766ba72783e7462e08d6d29a5d47e3b75c8ee3142485210",
            "size": "13940828"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-bootstrap-0.9.1.tar.xz",
            "shasum": "0a8e221c71860d8975c15662b3ed3bd863e81c4fe383455a596e5e0e490d6109",
            "size": "42488812"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-freebsd-x86_64-0.9.1.tar.xz",
            "shasum": "4e06009bd3ede34b72757eec1b5b291b30aa0d5046dadd16ecb6b34a02411254",
            "size": "39028848"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-linux-aarch64-0.9.1.tar.xz",
            "shasum": "5d99a39cded1870a3fa95d4de4ce68ac2610cca440336cfd252ffdddc2b90e66",
            "size": "37034860"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-linux-armv7a-0.9.1.tar.xz",
            "shasum": "6de64456cb4757a555816611ea697f86fba7681d8da3e1863fa726a417de49be",
            "size": "37974652"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-linux-i386-0.9.1.tar.xz",
            "shasum": "e776844fecd2e62fc40d94718891057a1dbca1816ff6013369e9a38c874374ca",
            "size": "44969172"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-linux-riscv64-0.9.1.tar.xz",
            "shasum": "208dea53662c2c52777bd9e3076115d2126a4f71aed7f2ff3b8fe224dc3881aa",
            "size": "39390868"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-linux-x86_64-0.9.1.tar.xz",
            "shasum": "be8da632c1d3273f766b69244d80669fe4f5e27798654681d77c992f17c237d7",
            "size": "41011464"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-macos-aarch64-0.9.1.tar.xz",
            "shasum": "8c473082b4f0f819f1da05de2dbd0c1e891dff7d85d2c12b6ee876887d438287",
            "size": "38995640"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-macos-x86_64-0.9.1.tar.xz",
            "shasum": "2d94984972d67292b55c1eb1c00de46580e9916575d083003546e9a01166754c",
            "size": "43713044"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-windows-i386-0.9.1.zip",
            "shasum": "74a640ed459914b96bcc572183a8db687bed0af08c30d2ea2f8eba03ae930f69",
            "size": "67929868"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-windows-x86_64-0.9.1.zip",
            "shasum": "443da53387d6ae8ba6bac4b3b90e9fef4ecbe545e1c5fa3a89485c36f5c0e3a2",
            "size": "65047697"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/download/0.9.1/zig-windows-aarch64-0.9.1.zip",
            "shasum": "621bf95f54dc3ff71466c5faae67479419951d7489e40e87fd26d195825fb842",
            "size": "61478151"
        }
    },
    "0.9.0": {
        "date": "2021-12-20",
        "docs": "https://ziglang.org/documentation/0.9.0/",
        "stdDocs": "https://ziglang.org/documentation/0.9.0/std/",
        "notes": "https://ziglang.org/download/0.9.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-0.9.0.tar.xz",
            "shasum": "cd1be83b12f8269cc5965e59877b49fdd8fa638efb6995ac61eb4cea36a2e381",
            "size": "13928772"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-bootstrap-0.9.0.tar.xz",
            "shasum": "16b0bdf0bc0a5ed1e0950e08481413d806192e06443a512347526647b2baeabc",
            "size": "42557736"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-freebsd-x86_64-0.9.0.tar.xz",
            "shasum": "c95afe679b7cc4110dc2ecd3606c83a699718b7a958d6627f74c20886333e194",
            "size": "41293236"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-linux-aarch64-0.9.0.tar.xz",
            "shasum": "1524fedfdbade2dbc9bae1ed98ad38fa7f2114c9a3e94da0d652573c75efbc5a",
            "size": "40008396"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-linux-armv7a-0.9.0.tar.xz",
            "shasum": "50225dee6e6448a63ee96383a34d9fe3bba34ae8da1a0c8619bde2cdfc1df87d",
            "size": "41196876"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-linux-i386-0.9.0.tar.xz",
            "shasum": "b0dcf688349268c883292acdd55eaa3c13d73b9146e4b990fad95b84a2ac528b",
            "size": "47408656"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-linux-riscv64-0.9.0.tar.xz",
            "shasum": "85466de07504767ed37f59782672ad41bbdf43d6480fafd07f45543278b07620",
            "size": "44171420"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-linux-x86_64-0.9.0.tar.xz",
            "shasum": "5c55344a877d557fb1b28939785474eb7f4f2f327aab55293998f501f7869fa6",
            "size": "43420796"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-macos-aarch64-0.9.0.tar.xz",
            "shasum": "3991c70594d61d09fb4b316157a7c1d87b1d4ec159e7a5ecd11169ff74cad832",
            "size": "39013392"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-macos-x86_64-0.9.0.tar.xz",
            "shasum": "c5280eeec4d6e5ea5ce5b448dc9a7c4bdd85ecfed4c1b96aa0835e48b36eccf0",
            "size": "43764596"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-windows-i386-0.9.0.zip",
            "shasum": "bb839434afc75092015cf4c33319d31463c18512bc01dd719aedf5dcbc368466",
            "size": "67946715"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-windows-x86_64-0.9.0.zip",
            "shasum": "084ea2646850aaf068234b0f1a92b914ed629be47075e835f8a67d55c21d880e",
            "size": "65045849"
        },
        "aarch64-windows": {
            "tarball": "https://ziglang.org/download/0.9.0/zig-windows-aarch64-0.9.0.zip",
            "shasum": "f9018725e3fb2e8992b17c67034726971156eb190685018a9ac8c3a9f7a22340",
            "size": "61461921"
        }
    },
    "0.8.1": {
        "date": "2021-09-06",
        "docs": "https://ziglang.org/documentation/0.8.1/",
        "stdDocs": "https://ziglang.org/documentation/0.8.1/std/",
        "notes": "https://ziglang.org/download/0.8.1/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-0.8.1.tar.xz",
            "shasum": "8c428e14a0a89cb7a15a6768424a37442292858cdb695e2eb503fa3c7bf47f1a",
            "size": "12650228"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-bootstrap-0.8.1.tar.xz",
            "shasum": "fa1239247f830ecd51c42537043f5220e4d1dfefdc54356fa419616a0efb3902",
            "size": "43613464"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-freebsd-x86_64-0.8.1.tar.xz",
            "shasum": "fc4f6478bcf3a9fce1b8ef677a91694f476dd35be6d6c9c4f44a8b76eedbe176",
            "size": "39150924"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-linux-aarch64-0.8.1.tar.xz",
            "shasum": "2166dc9f2d8df387e8b4122883bb979d739281e1ff3f3d5483fec3a23b957510",
            "size": "37605932"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-linux-armv7a-0.8.1.tar.xz",
            "shasum": "5ba58141805e2519f38cf8e715933cbf059f4f3dade92c71838cce341045de05",
            "size": "39185876"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-linux-i386-0.8.1.tar.xz",
            "shasum": "2f3e84f30492b5f1c5f97cecc0166f07a8a8d50c5f85dbb3a6ef2a4ee6f915e6",
            "size": "44782932"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-linux-riscv64-0.8.1.tar.xz",
            "shasum": "4adfaf147b025917c03367462fe5018aaa9edbc6439ef9cd0da2b074ae960554",
            "size": "41234480"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-linux-x86_64-0.8.1.tar.xz",
            "shasum": "6c032fc61b5d77a3f3cf781730fa549f8f059ffdb3b3f6ad1c2994d2b2d87983",
            "size": "41250060"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-macos-aarch64-0.8.1.tar.xz",
            "shasum": "5351297e3b8408213514b29c0a938002c5cf9f97eee28c2f32920e1227fd8423",
            "size": "35340712"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-macos-x86_64-0.8.1.tar.xz",
            "shasum": "16b0e1defe4c1807f2e128f72863124bffdd906cefb21043c34b673bf85cd57f",
            "size": "39946200"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-windows-i386-0.8.1.zip",
            "shasum": "099605051eb0452a947c8eab8fbbc7e43833c8376d267e94e41131c289a1c535",
            "size": "64152358"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.8.1/zig-windows-x86_64-0.8.1.zip",
            "shasum": "43573db14cd238f7111d6bdf37492d363f11ecd1eba802567a172f277d003926",
            "size": "61897838"
        }
    },
    "0.8.0": {
        "date": "2021-06-04",
        "docs": "https://ziglang.org/documentation/0.8.0/",
        "stdDocs": "https://ziglang.org/documentation/0.8.0/std/",
        "notes": "https://ziglang.org/download/0.8.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-0.8.0.tar.xz",
            "shasum": "03a828d00c06b2e3bb8b7ff706997fd76bf32503b08d759756155b6e8c981e77",
            "size": "12614896"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-bootstrap-0.8.0.tar.xz",
            "shasum": "10600bc9c01f92e343f40d6ecc0ad05d67d27c3e382bce75524c0639cd8ca178",
            "size": "43574248"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-freebsd-x86_64-0.8.0.tar.xz",
            "shasum": "0d3ccc436c8c0f50fd55462f72f8492d98723c7218ffc2a8a1831967d81b4bdc",
            "size": "39125332"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-linux-aarch64-0.8.0.tar.xz",
            "shasum": "ee204ca2c2037952cf3f8b10c609373a08a291efa4af7b3c73be0f2b27720470",
            "size": "37575428"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-linux-armv7a-0.8.0.tar.xz",
            "shasum": "d00b8bd97b79f45d6f5da956983bafeaa082e6c2ae8c6e1c6d4faa22fa29b320",
            "size": "38884212"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-linux-i386-0.8.0.tar.xz",
            "shasum": "96e43ee6ed81c3c63401f456bd1c58ee6d42373a43cb324f5cf4974ca0998865",
            "size": "42136032"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-linux-riscv64-0.8.0.tar.xz",
            "shasum": "75997527a78cdab64c40c43d9df39c01c4cdb557bb3992a869838371a204cfea",
            "size": "40016268"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-linux-x86_64-0.8.0.tar.xz",
            "shasum": "502625d3da3ae595c5f44a809a87714320b7a40e6dff4a895b5fa7df3391d01e",
            "size": "41211184"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-macos-aarch64-0.8.0.tar.xz",
            "shasum": "b32d13f66d0e1ff740b3326d66a469ee6baddbd7211fa111c066d3bd57683111",
            "size": "35292180"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-macos-x86_64-0.8.0.tar.xz",
            "shasum": "279f9360b5cb23103f0395dc4d3d0d30626e699b1b4be55e98fd985b62bc6fbe",
            "size": "39969312"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-windows-i386-0.8.0.zip",
            "shasum": "b6ec9aa6cd6f3872fcb30d43ff411802d82008a0c4142ee49e208a09b2c1c5fe",
            "size": "61507213"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.8.0/zig-windows-x86_64-0.8.0.zip",
            "shasum": "8580fbbf3afb72e9b495c7f8aeac752a03475ae0bbcf5d787f3775c7e1f4f807",
            "size": "61766193"
        }
    },
    "0.7.1": {
        "date": "2020-12-13",
        "docs": "https://ziglang.org/documentation/0.7.1/",
        "stdDocs": "https://ziglang.org/documentation/0.7.1/std/",
        "notes": "https://ziglang.org/download/0.7.1/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-0.7.1.tar.xz",
            "shasum": "2db3b944ab368d955b48743d9f7c963b8f96de1a441ba5a35e197237cc6dae44",
            "size": "10711824"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-bootstrap-0.7.1.tar.xz",
            "shasum": "040f27c1fae4b0cac0a2782aecdb691f6a2f8e89db6a6ed35024c31c304fd9b2",
            "size": "40232612"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-freebsd-x86_64-0.7.1.tar.xz",
            "shasum": "e73c1dca35791a3183fdd5ecde0443ebbe180942efceafe651886034fb8def09",
            "size": "39066808"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-linux-aarch64-0.7.1.tar.xz",
            "shasum": "48ec90eba407e4587ddef7eecef25fec7e13587eb98e3b83c5f2f5fff2a5cbe7",
            "size": "33780552"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-linux-armv7a-0.7.1.tar.xz",
            "shasum": "5a0662e07b4c4968665e1f97558f8591f6facec45d2e0ff5715e661743107ceb",
            "size": "35813504"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-linux-i386-0.7.1.tar.xz",
            "shasum": "4882e052e5f83690bd0334bb4fc1702b5403cb3a3d2aa63fd7d6043d8afecba3",
            "size": "39230912"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-linux-riscv64-0.7.1.tar.xz",
            "shasum": "187294bfd35983348c3fe042901b42e67e7e36ab7f77a5f969d21c0051f4d21f",
            "size": "37454812"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-linux-x86_64-0.7.1.tar.xz",
            "shasum": "18c7b9b200600f8bcde1cd8d7f1f578cbc3676241ce36d771937ce19a8159b8d",
            "size": "37848176"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-macos-x86_64-0.7.1.tar.xz",
            "shasum": "845cb17562978af0cf67e3993f4e33330525eaf01ead9386df9105111e3bc519",
            "size": "36211076"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-windows-i386-0.7.1.zip",
            "shasum": "a1b9a7421e13153e07fd2e2c93ff29aad64d83105b8fcdafa633dbe689caf1c0",
            "size": "54374983"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.7.1/zig-windows-x86_64-0.7.1.zip",
            "shasum": "4818a8a65b4672bc52c0ae7f14d014e0eb8caf10f12c0745176820384cea296a",
            "size": "54909997"
        }
    },
    "0.7.0": {
        "date": "2020-11-08",
        "docs": "https://ziglang.org/documentation/0.7.0/",
        "stdDocs": "https://ziglang.org/documentation/0.7.0/std/",
        "notes": "https://ziglang.org/download/0.7.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-0.7.0.tar.xz",
            "shasum": "0efd2cf6c3b05723db80e9cf193bc55150bba84ca41f855a90f53fc756445f83",
            "size": "10683920"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-bootstrap-0.7.0.tar.xz",
            "shasum": "f073beaf5c53c8c57c0d374cbfcb332ef92ad703173edba0d9e0f2ed28401b72",
            "size": "40200436"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-freebsd-x86_64-0.7.0.tar.xz",
            "shasum": "a0c926272ee4ae720034b4a6a1dc98399d76156dd84182554740f0ca8a41fc99",
            "size": "34798992"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-linux-aarch64-0.7.0.tar.xz",
            "shasum": "f89933bac87d44be82325754ff88423020c81c7032a6fc41cfeb81e982eeab9b",
            "size": "33096140"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-linux-armv7a-0.7.0.tar.xz",
            "shasum": "011c267e25a96ee160505a560c441daa045359a9d50e13ab1bada9d75c95db2d",
            "size": "35157584"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-linux-i386-0.7.0.tar.xz",
            "shasum": "4bb2072cd363bcb1cbeb4872ff5cbc1f683b02d0cc1f90c46e3ea7422ce53222",
            "size": "38530596"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-linux-riscv64-0.7.0.tar.xz",
            "shasum": "40dff81faa6f232ac40abbf88b9371f3cc932b6e09c423b94387c9ea580cb7be",
            "size": "36759992"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-linux-x86_64-0.7.0.tar.xz",
            "shasum": "e619b1c6094c095b932767f527aee2507f847ea981513ff8a08aab0fd730e0ac",
            "size": "37154432"
        },
        "aarch64-macos": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-macos-aarch64-0.7.0.tar.xz",
            "shasum": "338238035734db74ea4f30e500a4893bf741d38305c10952d5e39fa05bdb057d",
            "size": "33739424"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-macos-x86_64-0.7.0.tar.xz",
            "shasum": "94063f9a311cbbf7a2e0a12295e09437182cf950f18cb0eb30ea9893f3677f24",
            "size": "35258328"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-windows-i386-0.7.0.zip",
            "shasum": "b1e520aacbfbd645ff3521b3eb4d44166d9a0288b8725e4b001f8b50a425eb2e",
            "size": "53390517"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.7.0/zig-windows-x86_64-0.7.0.zip",
            "shasum": "965f56c0a36f9cda2125e3a348bc654f7f155e2804c3667d231775ec228f8553",
            "size": "53943784"
        }
    },
    "0.6.0": {
        "date": "2020-04-13",
        "docs": "https://ziglang.org/documentation/0.6.0/",
        "stdDocs": "https://ziglang.org/documentation/0.6.0/std/",
        "notes": "https://ziglang.org/download/0.6.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-0.6.0.tar.xz",
            "shasum": "5d167dc19354282dd35dd17b38e99e1763713b9be8a4ba9e9e69284e059e7204",
            "size": "10349552"
        },
        "bootstrap": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-bootstrap-0.6.0.tar.xz",
            "shasum": "5e0e4dc878b3dd0c1852a442b174f0732e8c07869a8fcd226b71a93b89b381ab",
            "size": "38469948"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-freebsd-x86_64-0.6.0.tar.xz",
            "shasum": "190ff79c1eb56805a315d7c7a51082e32f62926250c0702b36760c225e1634a3",
            "size": "36974604"
        },
        "aarch64-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-aarch64-0.6.0.tar.xz",
            "shasum": "e7520efd42cfa02be48c2e430d08fe1f3cbb999d21d9f0d3ffd0febb976b2f41",
            "size": "37090044"
        },
        "armv6kz-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-armv6kz-0.6.0.tar.xz",
            "shasum": "36b6493b3fed43eb1f0000e765798ad31a6bb7d7fd3f553ac1c3761dbc919b82",
            "size": "39133452"
        },
        "armv7a-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-armv7a-0.6.0.tar.xz",
            "shasum": "946969abe357def95ca9cbbfcebfcf2d90cf967bcd3f48ee87662e32d91d8f35",
            "size": "39143748"
        },
        "i386-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-i386-0.6.0.tar.xz",
            "shasum": "a97a2f9ae21575743cdd763c1917d49400d83fc562ef64582b18bade43eb24ce",
            "size": "44877640"
        },
        "riscv64-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-riscv64-0.6.0.tar.xz",
            "shasum": "68ddee43f7503c8ae5f26a921f3602c34719a02ed2241f528c0b8b888cc14b38",
            "size": "41993144"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-linux-x86_64-0.6.0.tar.xz",
            "shasum": "08fd3c757963630645441c2772362e9c2294020c44f14fce1b89f45de0dc1253",
            "size": "44766320"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-macos-x86_64-0.6.0.tar.xz",
            "shasum": "17270360e87ddc49f737e760047b2fac49f1570a824a306119b1194ac4093895",
            "size": "42573184"
        },
        "i386-windows": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-windows-i386-0.6.0.zip",
            "shasum": "3b0a02618743e92175990dc6d1a787bb95ff62c4cda016f1c14c7786f575f8ca",
            "size": "60446431"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.6.0/zig-windows-x86_64-0.6.0.zip",
            "shasum": "c3b897832523e1026e10b2d8d55d7f895185c0a27a63681f3a23219c3f1c38f4",
            "size": "49065511"
        }
    },
    "0.5.0": {
        "date": "2019-09-30",
        "docs": "https://ziglang.org/documentation/0.5.0/",
        "notes": "https://ziglang.org/download/0.5.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.5.0/zig-0.5.0.tar.xz",
            "shasum": "55ae16960f152bcb9cf98b4f8570902d0e559a141abf927f0d3555b7cc838a31",
            "size": "10956132"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.5.0/zig-freebsd-x86_64-0.5.0.tar.xz",
            "shasum": "9e1f4d36c3d584c0aa01f20eb4cd0a0eef3eee5af23e483b8414de55feab6ab6",
            "size": "33650744"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.5.0/zig-macos-x86_64-0.5.0.tar.xz",
            "shasum": "28702cc05745c7c0bd450487d5f4091bf0a1ad279b35eb9a640ce3e3a15b300d",
            "size": "37898664"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.5.0/zig-windows-x86_64-0.5.0.zip",
            "shasum": "58141323db8d84a5af62746be5f9140bc161ee760ef33dc91a887bf9ac021976",
            "size": "44871804"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.5.0/zig-linux-x86_64-0.5.0.tar.xz",
            "shasum": "43e8f8a8b8556edd373ddf9c1ef3ca6cf852d4d09fe07d5736d12fefedd2b4f7",
            "size": "40895068"
        }
    },
    "0.4.0": {
        "date": "2019-04-08",
        "docs": "https://ziglang.org/documentation/0.4.0/",
        "notes": "https://ziglang.org/download/0.4.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.4.0/zig-0.4.0.tar.xz",
            "shasum": "fec1f3f6b359a3d942e0a7f9157b3b30cde83927627a0e1ea95c54de3c526cfc",
            "size": "5348776"
        },
        "x86_64-freebsd": {
            "tarball": "https://ziglang.org/download/0.4.0/zig-freebsd-x86_64-0.4.0.tar.xz",
            "shasum": "3d557c91ac36d8262eb1733bb5f261c95944f9b635e43386e3d00a3272818c30",
            "size": "27269672"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.4.0/zig-macos-x86_64-0.4.0.tar.xz",
            "shasum": "67c932982484d017c5111e54af9f33f15e8e05c6bc5346a55e04052159c964a8",
            "size": "30841504"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.4.0/zig-windows-x86_64-0.4.0.zip",
            "shasum": "fbc3dd205e064c263063f69f600bedb18e3d0aa2efa747a63ef6cafb6d73f127",
            "size": "35800101"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.4.0/zig-linux-x86_64-0.4.0.tar.xz",
            "shasum": "fb1954e2fb556a01f8079a08130e88f70084e08978ff853bb2b1986d8c39d84e",
            "size": "32876100"
        }
    },
    "0.3.0": {
        "date": "2018-09-28",
        "docs": "https://ziglang.org/documentation/0.3.0/",
        "notes": "https://ziglang.org/download/0.3.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.3.0/zig-0.3.0.tar.xz",
            "shasum": "d70af604f3a8622f3393d93abb3e056bf60351e32d121e6fa4fe03d8d41e1f5a",
            "size": "2335592"
        },
        "x86_64-macos": {
            "tarball": "https://ziglang.org/download/0.3.0/zig-macos-x86_64-0.3.0.tar.xz",
            "shasum": "19dec1f1943ab7be26823376d466f7e456143deb34e17502778a949034dc2e7e",
            "size": "23712696"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.3.0/zig-windows-x86_64-0.3.0.zip",
            "shasum": "bb568c03950958f8bb3472139c3ab5ed74547c8c694ab50f404c202faf51baf4",
            "size": "22524425"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.3.0/zig-linux-x86_64-0.3.0.tar.xz",
            "shasum": "b378d0aae30cb54f28494e7bc4efbc9bfb6326f47bfb302e8b5287af777b2f3c",
            "size": "25209304"
        }
    },
    "0.2.0": {
        "date": "2018-03-15",
        "docs": "https://ziglang.org/documentation/0.2.0/",
        "notes": "https://ziglang.org/download/0.2.0/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.2.0/zig-0.2.0.tar.xz",
            "shasum": "29c9beb172737f4d5019b88ceae829ae8bc6512fb4386cfbf895ae2b42aa6965",
            "size": "1940832"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.2.0/zig-win64-0.2.0.zip",
            "shasum": "4f8a2979941a1f081ec8e545cca0b72608c0db1c5a3fd377a94db40649dcd3d4",
            "size": "21076274"
        },
        "x86_64-linux": {
            "tarball": "https://ziglang.org/download/0.2.0/zig-linux-x86_64-0.2.0.tar.xz",
            "shasum": "209c6fb745d42474c0a73d6f291c7ae3a38b6a1b6b641eea285a7f840cc1a890",
            "size": "22551928"
        }
    },
    "0.1.1": {
        "date": "2017-10-17",
        "docs": "https://ziglang.org/documentation/0.1.1/",
        "notes": "https://ziglang.org/download/0.1.1/release-notes.html",
        "src": {
            "tarball": "https://ziglang.org/download/0.1.1/zig-0.1.1.tar.xz",
            "shasum": "ffca0cfb263485287e19cc997b08701fcd5f24b700345bcdc3dd8074f5a104e0",
            "size": "1659716"
        },
        "x86_64-windows": {
            "tarball": "https://ziglang.org/download/0.1.1/zig-win64-0.1.1.zip",
            "shasum": "6fc88bef531af7e567fe30bf60da1487b86833cbee84c7a2f3e317030aa5b660",
            "size": "19757776"
        }
    }
}
