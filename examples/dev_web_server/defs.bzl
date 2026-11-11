# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This source code is dual-licensed under either the MIT license found in the
# LICENSE-MIT file in the root directory of this source tree or the Apache
# License, Version 2.0 found in the LICENSE-APACHE file in the root directory
# of this source tree. You may select, at your option, one of the
# above-listed licenses.

ThemeInfo = provider(
    fields = {
        "files": provider_field(dict[str, Artifact]),
        "footer": provider_field(Artifact),
        "header": provider_field(Artifact),
    }
)

def _theme_impl(ctx):
    return [
        DefaultInfo(),
        ThemeInfo(
            header = ctx.attrs.header,
            footer = ctx.attrs.footer,
            files = {f.basename: f for f in ctx.attrs.files},
        ),
    ]

# A page header and footer, and files such as style sheets that every page uses.
theme = rule(
    impl = _theme_impl,
    attrs = {
        "files": attrs.list(attrs.source(), default = []),
        "footer": attrs.source(),
        "header": attrs.source(),
    },
)

def _page(ctx, theme, body):
    """Put the theme's header and footer around a page body."""
    name = body.basename
    out = ctx.actions.declare_output("pages", name)
    ctx.actions.run(
        cmd_args(
            "sh",
            "-c",
            'cat "$1" "$2" "$3" > "$4"',
            "--",
            theme.header,
            body,
            theme.footer,
            out.as_output(),
        ),
        category = "page",
        identifier = name,
    )
    return name, out

def _static_site_impl(ctx):
    theme = ctx.attrs.theme[ThemeInfo]
    files = dict(theme.files)
    for body in ctx.attrs.pages:
        name, out = _page(ctx, theme, body)
        files[name] = out
    www = ctx.actions.copied_dir("www", files)

    server = cmd_args(
        "python3",
        ctx.attrs.server[DefaultInfo].default_outputs[0],
        "--port",
        str(ctx.attrs.port),
    )
    return [
        DefaultInfo(default_output = www),
        RunInfo(args = cmd_args(server, "--root", www)),
        DevInfo(
            # `buck2 dev` starts the server, and starts it again only when the server script
            # changes: the command names nothing else. The site is not in the command. It is
            # an output, which the server asks for when it starts, and it serves the newest
            # one it has been told about. After a change to anything that it may be built
            # from, a page or the theme, `buck2 dev` says that it is stale, and the server
            # asks for it again.
            server = server,
            outputs = {"www": www},
        ),
    ]

# A directory of HTML pages, made from page bodies and a theme. `buck2 dev` serves it and
# rebuilds it as the files change.
static_site = rule(
    impl = _static_site_impl,
    attrs = {
        "pages": attrs.list(attrs.source()),
        "port": attrs.int(default = 8000),
        "server": attrs.dep(),
        "theme": attrs.dep(providers = [ThemeInfo]),
    },
)
