import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

export function render(text) {
  return renderToStaticMarkup(createElement("p", null, text));
}
