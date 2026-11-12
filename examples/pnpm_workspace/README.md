# A pnpm workspace

`pnpm-lock.yaml` is the only description of the npm dependencies: buck2 loads it
as data, so there is no BUCK file to regenerate. After `pnpm add` or
`pnpm install --lockfile-only`, the next build uses the new lockfile.

```sh
buck2 run //packages/cli:main
```

- `PACKAGE` records the workspace for the packages below it
  (`pnpm_workspace_package`).
- `BUCK` defines `:npm`, a `nodejs_npm_store` with one entry per package
  snapshot in the lockfile, and `:npm/packages/<name>`, the npm dependencies of
  each workspace package (`pnpm_workspace`).
- Each `packages/<name>/BUCK` defines `:<name>`, a `nodejs_library` whose
  workspace and npm dependencies come from the lockfile (`pnpm_package`).

Every package sees only what its `package.json` declares, as with pnpm:
`@example/greet` imports `chalk`, and `@example/cli`, which depends on
`@example/greet` but not on `chalk`, cannot. Tarballs are downloaded and
extracted only for the packages a build needs.
