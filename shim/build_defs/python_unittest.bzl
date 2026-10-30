# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

load("@prelude//utils:type_defs.bzl", "is_select")
load("@shim//build_defs/lib:oss.bzl", "default_base_module", "translate_target")

def _translate_env_value_str(v):
    # Rewrite any cell-qualified targets that appear inside `$(macro ...)`
    # substrings (e.g. `$(location fbcode//buck2:buck2)`) so that env values
    # produced by the Meta-internal `tests/buck_e2e.bzl` resolve correctly
    # through the OSS shim cell mapping.
    if "//" not in v:
        return v
    chunks = v.split("$(")
    out = [chunks[0]]
    for chunk in chunks[1:]:
        if ")" not in chunk:
            out.append("$(" + chunk)
            continue
        end = chunk.index(")")
        macro_body = chunk[:end]
        rest = chunk[end:]
        tokens = macro_body.split(" ")
        for i in range(len(tokens)):
            if "//" in tokens[i]:
                tokens[i] = translate_target(tokens[i])
        out.append("$(" + " ".join(tokens) + rest)
    return "".join(out)

def _translate_env_value(v):
    if is_select(v):
        return select_map(v, _translate_env_value_str)
    return _translate_env_value_str(v)

def python_unittest(
    name,
    srcs = [],
    py_version = None,
    base_module = None,
    deps = None,
    env = None,
    emails = None,
    resources = None,
    skip_on_mode_mac = False,
    skip_on_mode_win = False,
    modifiers = None,
    network_access = None,
    remote_execution = None,
    supports_static_listing = None,
    **kwargs,
):
    # The prelude `python_test` rule has no equivalent of these fbcode macro
    # attributes; tests run on all platforms, locally, in OSS.
    _unused = (
        # @unused
        py_version,
        skip_on_mode_mac,
        skip_on_mode_win,
        modifiers,
        network_access,
        remote_execution,
        supports_static_listing,
    )

    if base_module == None:
        base_module = default_base_module()

    if deps != None:
        kwargs["deps"] = [translate_target(d) for d in deps]

    if resources != None:
        # The fbcode macro takes resources as `{target: dest_path}`; the prelude
        # `python_test` rule takes `{dest_path: target}`. Flip and rewrite the
        # targets to their OSS equivalents.
        kwargs["resources"] = {dest: translate_target(target) for target, dest in resources.items()}

    if env:
        kwargs["env"] = {k: _translate_env_value(v) for k, v in env.items()}

    if emails != None:
        kwargs["contacts"] = emails

    # The buck2 e2e tests look up `__manifest__.fbmake["build_rule"]` (a Meta
    # convention) to tell test binaries apart. Provide a minimally compatible
    # shape so tests that read it don't crash in OSS.
    kwargs.setdefault(
        "manifest_module_entries",
        {"fbmake": {"build_rule": "{}:{}".format(native.package_name(), name)}},
    )

    # @lint-ignore BUCKLINT: avoid "Direct usage of native rules is not allowed."
    native.python_test(name = name, srcs = srcs, base_module = base_module, **kwargs)
