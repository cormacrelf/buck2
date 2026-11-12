# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

# What vite.config.ts says about every package's stylesheets, for the buck2 build. Vite applies
# its config to every module of the app, those of packages/ui too, so each `vite_package` takes
# these.

load(":package.json", _package = "value")

PACKAGE = _package

STYLE = {
    "browserslist": _package["browserslist"],
    "css_modules_camel_case_only": True,
    # `[name]__[local]__[hash:base64:4]` in vite.config.ts; lightningcss has no hash lengths.
    "css_modules_pattern": "[name]__[local]__[hash]",
}
