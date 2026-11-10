# A web server that `buck2 dev` keeps up to date

This example builds a small static site and serves it with `buck2 dev`. When you
edit a page or the theme, `buck2 dev` builds the site again, and the server
serves the new files from the next request. Reload the browser to see the
change. The server does not push changes to the browser.

```sh
buck2 dev //site:site
```

Open <http://localhost:8000/>. To use another port, set `PORT`:
`PORT=9000 buck2 dev //site:site`.

`buck2 run //site:site` serves one build of the site, without rebuilding it.

## Files

- `defs.bzl`: the `theme` and `static_site` rules. `static_site` returns the
  `DevInfo` provider that `buck2 dev` reads.
- `site/`: the page bodies. Each page is a body put between the theme's header
  and footer.
- `theme/`: the header, the footer and the style sheet.
- `server/server.py`: the web server. It asks `buck2 dev` for the `www` output
  when it starts, and serves the newest `www` directory that `buck2 dev` reports.
- `toolchains/`: the prelude needs a `toolchains` cell. This example's rules do
  not use any toolchain.

## What `buck2 dev` does here

The server's command names only `server/server.py`. `buck2 dev` builds the
command again after each change, and when `server.py` has new contents it stops
the server and starts it again. The site is not in the command: it is the `www`
output, which the server asks for when it starts.

`static_site` gives `buck2 dev` one watch set: a change in the dependency
graph, to a page or to the theme, builds the `www` output again and tells the
server its new path.

See the rule author documentation page for `buck2 dev` for the messages between
`buck2 dev` and the server.
