# Recursive anon targets: giving anon targets the power of Bazel aspects

Tracking issue: <https://github.com/facebook/buck2/issues/799>

## Problem

Anon targets are documented as buck2's answer to Bazel
[aspects](https://bazel.build/extending/aspects) — the "overlay problem" of
building a shadow-graph over the real target graph
(`docs/rule_authors/anon_targets.md:11-22`). In practice they only solve it one
level deep: an anon rule can shadow a target, but it **cannot recurse into that
target's dependencies**, so you cannot build a shadow-graph over an entire
dependency tree.

The single fact that causes this: a normal (non-BXL) anon rule impl receives a
full `AnalysisContext` (`app/buck2_anon_target/src/anon_targets.rs:524`), but the
only handle it has on a dependency is a `Dependency` /
`FrozenProviderCollection` (`app/buck2_build_api/src/interpreter/rule_defs/provider/dependency.rs:71`).
That object exposes `.label`, `.providers`, `.get()`, and `.sub_target()`
(`dependency.rs:207`) — **nothing that enumerates the dependency's own edges**.
Query is explicitly refused during analysis (`QueryMacroNotSupported`,
`app/buck2_anon_target/src/anon_target_attr_resolve.rs:213`). This is deliberate:
`anon_targets.md:24-32` withholds fully-general "look at a result, then ask for
fresh things" so that `query` and the Target Determinator keep working.

So the entire problem reduces to one question:

> How does an anon rule impl learn the edges it needs in order to recurse?

Two answers follow. Both get one property for free: anon targets are
content-keyed DICE keys (`AnonTargetKey: Key`, `anon_targets.rs:154`), so
recursion over a DAG is automatically memoized and deduplicated — no manual
visited-set, and shared subgraphs are analyzed once.

By contrast, transitive sets (tsets) — buck2's normal mechanism for propagating
data up the graph — require **every rule** to explicitly read its deps' providers
and re-emit its own tset (`prelude/linking/link_info.bzl:542-615` is the
canonical shape). That is exactly the "modify 100 rules" cost the overlay is
meant to avoid.

---

## Option 1 — Userland overlay via a single edge-provider

**Zero core (Rust) changes. Prototype this first.**

The instinct that "you'd have to modify every rule" assumes each rule emits *your
specific* tset. Invert it: get every rule to emit **its edges, once**, via one
convention provider, and keep the aspect logic in a single library.

There is a real single chokepoint for injecting that provider: `_mk_rule` in
`prelude/rules.bzl:26` wraps every prelude rule's impl. Wrap it so each rule
*also* returns an `AspectDepsInfo`.

### Sketch

```python
# prelude/aspect.bzl

AspectDepsInfo = provider(fields = ["deps"])  # deps: list[Dependency]

def _collect_dep_attrs(ctx):
    # Scan resolved attrs for anything that is (a list of) Dependency.
    # No need to introspect the attr *spec* — inspect the resolved values.
    out = []
    for name in dir(ctx.attrs):
        v = getattr(ctx.attrs, name)
        if type(v) == "dependency":
            out.append(v)
        elif type(v) == "list":
            out.extend([x for x in v if type(x) == "dependency"])
    return out
```

In `prelude/rules.bzl`, wrap the impl produced by `_mk_rule` so its returned
provider list gains `AspectDepsInfo(deps = _collect_dep_attrs(ctx))`. Every
prelude rule now exposes its edges with no per-rule edits.

The aspect itself is an ordinary anon rule that recurses:

```python
# prelude/aspect.bzl (cont.)

MyAspectResult = provider(fields = ["tset"])
MyTSet = transitive_set()

def _aspect_impl(ctx):
    target = ctx.attrs.target                      # Dependency
    edges = target[AspectDepsInfo].deps            # list[Dependency]

    # Recurse: one anon target per edge. DICE memoizes shared subgraphs.
    promises = [
        ctx.actions.anon_target(aspect_rule, {"target": dep})
        for dep in edges
    ]

    def _finish(child_providers):
        children = [p[MyAspectResult].tset for p in child_providers]
        value = _compute_my_overlay(target)        # whatever the aspect extracts
        tset = ctx.actions.tset(MyTSet, value = value, children = children)
        return [DefaultInfo(), MyAspectResult(tset = tset)]

    return ctx.actions.anon_targets(promises).promise.map(_finish)

aspect_rule = anon_rule(
    impl = _aspect_impl,
    attrs = {"target": attrs.dep()},
    artifact_promise_mappings = {},
)
```

The `Dependency` read out of `AspectDepsInfo.deps` is fed straight back into
`attrs.dep()`; coercion accepts a `Dependency` value
(`app/buck2_anon_target/src/anon_target_attr_coerce.rs:96`). Anon targets take
already-configured deps and forbid transitions (`anon_target_attr_coerce.rs:216`),
so there is no configuration ambiguity in the recursion.

### Why this is the one to build first

- **Works mid-graph.** Any rule can instantiate the aspect over its own deps —
  the thing the BXL workaround (issue comment 1) fundamentally cannot do, since
  BXL only drives from the top.
- **No compiler changes**, so it ships immediately and unblocks the concrete
  users (Tailwind source collection; snowydeer nix-dep collection).
- **Validates the ergonomics** before committing to a core primitive.

### Limitations

- Requires the `_mk_rule` wrapper + the `AspectDepsInfo` convention; a rule
  outside the prelude that doesn't route through `_mk_rule` won't expose edges
  unless it opts in.
- `_collect_dep_attrs` scans all attrs at impl time (cheap, but not free) and
  keys off `type(v) == "dependency"`; exotic dep-bearing shapes (deps nested in
  dicts, etc.) need explicit handling.
- There is no universal base rule in the prelude — deps handling is per-language
  (e.g. `cxx_attr_deps` at `prelude/cxx/cxx_library_utility.bzl:38`) — so
  `_mk_rule` is the closest thing to a global injection point, and it only covers
  rules assembled through it.

---

## Option 2 — Narrow core primitive: one-hop edge visibility on `Dependency`

**The durable feature. Removes Option 1's wrapper and convention provider.**

Option 1's only wart is the `_mk_rule` wrapper and the `AspectDepsInfo`
convention. Remove both by surfacing edges natively: add a `.deps` accessor to
`DependencyGen`
(`app/buck2_build_api/src/interpreter/rule_defs/provider/dependency.rs:71`)
returning the target's immediate dependency labels/deps.

buck2 already computes exactly this. Anon-target resolution walks a node's edges
via `AnonTargetDependents` / `ConfiguredAttrTraversal`
(`anon_target_attr_resolve.rs:188-211`) — the traversal machinery exists; this
change threads that **one hop** of edges onto the analysis result so a
`Dependency` can hand it back to Starlark.

With `.deps` on `Dependency`, the aspect no longer needs a convention provider:

```python
def _aspect_impl(ctx):
    target = ctx.attrs.target
    edges = target.deps                            # <-- native, no AspectDepsInfo
    promises = [
        ctx.actions.anon_target(aspect_rule, {"target": dep})
        for dep in edges
    ]
    return ctx.actions.anon_targets(promises).promise.map(_finish)
```

### Why this respects the isolation model

This is **not** query and **not** "ask for fresh things." It exposes edges buck2
*already traversed* for that node — one hop — which the impl re-instantiates as
anon targets (themselves memoized DICE keys). It does not give analysis the
ability to run arbitrary graph queries, so the `query` / Target Determinator
guarantees that `anon_targets.md:24-32` protects are preserved.

### Design questions to settle

- **What to return per edge:** a `Dependency` (immediately re-feedable into
  `attrs.dep()`) vs. a bare label (`attrs.label()`). A `Dependency` is the most
  ergonomic for recursion; a label is lighter if the caller only wants to
  re-key.
- **Which attrs count as edges:** all dep-typed attrs, or a way to scope to
  specific attributes (Bazel's `attr_aspects` names the attributes to propagate
  along). A scoping mechanism is the natural place aspects-proper (a possible
  future Option 3) would grow from.
- **Configured vs. unconfigured:** anon deps are already-configured and
  transition-free (`anon_target_attr_coerce.rs:216`), so `.deps` on an anon
  `Dependency` is unambiguous. Deciding whether `.deps` is offered on *all*
  `Dependency` objects (i.e. in normal rule analysis too) is a larger call and
  can be deferred.
- **Threading cost:** the analysis result must now carry immediate-dep info;
  measure the memory/keying impact before making it unconditional.

---

## Recommended sequencing

1. **Prototype Option 1 now** — unblocks Tailwind and snowydeer, no compiler
   changes, validates the aspect ergonomics and the DICE memoization behavior.
2. If it proves out, **land Option 2** as the durable primitive and delete the
   `_mk_rule` wrapper + `AspectDepsInfo` convention.

## Key code references

| Area | Location |
| --- | --- |
| `anon_target` / `anon_targets` methods | `app/buck2_anon_target/src/starlark_defs.rs:249` |
| Anon eval → full `AnalysisContext` | `app/buck2_anon_target/src/anon_targets.rs:524` |
| Anon target DICE key (memoization) | `app/buck2_anon_target/src/anon_targets.rs:154` |
| Allowed anon attr types (rule def) | `app/buck2_node/src/attrs/anon_target_attr_validation.rs:28` |
| Value coercion / `attrs.dep()` accepts `Dependency` | `app/buck2_anon_target/src/anon_target_attr_coerce.rs:56,96,216` |
| Dep resolution, edge traversal, query rejection | `app/buck2_anon_target/src/anon_target_attr_resolve.rs:188-213` |
| `Dependency` surface (no edge access today) | `app/buck2_build_api/src/interpreter/rule_defs/provider/dependency.rs:71,207` |
| Prelude rule assembly chokepoint | `prelude/rules.bzl:26` |
| Canonical tset aggregation pattern | `prelude/linking/link_info.bzl:542-615` |
| Overlay-problem framing vs. aspects | `docs/rule_authors/anon_targets.md:11-32` |
