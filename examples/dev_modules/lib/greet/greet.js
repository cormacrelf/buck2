import { shout } from "/lib/format/format.js";
import { names } from "/lib/greet/names.js";

export function greetings() {
  return names.map((name) => shout("hello, " + name));
}
