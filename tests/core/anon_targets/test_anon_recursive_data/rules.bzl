# @lint-ignore-every FBCODEBZLADDLOADS

JsFileCollection = provider(fields = ["files"])

def _js_module_impl(ctx: AnalysisContext) -> list[Provider]:
    return [DefaultInfo(), JsFileCollection(files = ctx.attrs.srcs)]

js_module = rule(
    impl = _js_module_impl,
    attrs = {
        "deps": attrs.list(attrs.dep(), default = []),
        "srcs": attrs.list(attrs.source(), default = []),
    },
)

def _collect_recursive_impl(ctx: AnalysisContext) -> Promise:
    target = ctx.attrs.target
    children = [
        (_collect_recursive, {"target": dep})
        for dep in target.deps
    ]

    def finish(child_results):
        files = list(target[JsFileCollection].files)
        for child in child_results:
            files.extend(child[JsFileCollection].files)
        return [DefaultInfo(), JsFileCollection(files = files)]

    return ctx.actions.anon_targets(children).promise.map(finish)

_collect_recursive = anon_rule(
    impl = _collect_recursive_impl,
    attrs = {"target": attrs.dep()},
    artifact_promise_mappings = {},
)

def _collect_js_sources_impl(ctx: AnalysisContext) -> Promise:
    promise = ctx.actions.anon_target(_collect_recursive, {"target": ctx.attrs.root}).promise

    def finish(result):
        output = ctx.actions.declare_output("all_js_sources.txt")
        ctx.actions.write(output, "".join([file.short_path + "\n" for file in result[JsFileCollection].files]))
        return [DefaultInfo(default_output = output)]

    return promise.map(finish)

collect_js_sources = rule(
    impl = _collect_js_sources_impl,
    attrs = {"root": attrs.dep(providers = [JsFileCollection])},
)
