# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is licensed under both the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree and the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree.

load(":common.bzl", "get_tagged_modifier", "tagged_modifier_to_json")
load(":set_cfg_constructor.bzl", "MODIFIER_METADATA_KEY")
load(":types.bzl", "Modifier", "ModifierPackageLocation")

def set_cfg_modifiers(modifiers: list[Modifier]):
    """
    Sets a configuration modifier for all targets under this PACKAGE file. This can only be called from a PACKAGE file context
    (e.g. a PACKAGE file or a bzl file transitively loaded by a PACKAGE file).

    Args:
        constraint_setting:
            The constraint key to modify. This must be a `constraint_setting` target.
            For example, to change the OS constraint setting in fbsource, this would be "ovr_config//os/constraints:os".
        modifier:
            The modifier to change this constraint setting. This can be a constraint value target.
            For example, to change the OS to linux in fbsource, this can be specified as "ovr_config//os/constraints:linux".
            TODO(scottcao): Add support for modifier select types.
    """

    merged_modifier_jsons = read_parent_package_value(MODIFIER_METADATA_KEY)

    # `read_parent_package_value` returns immutable values. `list()` makes it mutable.
    merged_modifier_jsons = list(merged_modifier_jsons) if merged_modifier_jsons else []

    merged_modifier_jsons += [
        tagged_modifier_to_json(get_tagged_modifier(
            modifier,
            ModifierPackageLocation(package_path = _get_package_path()),
        ))
        for modifier in modifiers
    ]

    write_package_value(
        MODIFIER_METADATA_KEY,
        merged_modifier_jsons,
        overwrite = True,
    )

def _get_package_path() -> str:
    """
    Returns the cell-relative path of the current PACKAGE file.
    Ex. `foo//bar/PACKAGE`
    """
    return "{}//{}/PACKAGE".format(get_cell_name(), get_base_path())
