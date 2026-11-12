import { greet } from "@example/greet";
import { render } from "@example/render";

console.log(greet("pnpm"));
console.log(render("rendered by react-dom"));

// Not a dependency of this package, only of @example/greet: it must not resolve here.
try {
  await import("chalk");
  console.log("chalk: resolved, but @example/cli does not depend on it");
  process.exitCode = 1;
} catch {
  console.log("chalk: not resolvable from @example/cli, as declared");
}
