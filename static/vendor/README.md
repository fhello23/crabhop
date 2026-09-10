# Text preview assets

These unmodified browser distributions are served locally. No CDN requests or
Node.js build step are needed to run Crabhop. Licenses are included alongside
the assets.

| Package | Version | Source file → local file |
| --- | --- | --- |
| [markdown-it](https://github.com/markdown-it/markdown-it) | 15.0.1 | `dist/browser/markdown-it.umd.min.js` → `markdown-it.min.js` |
| [@highlightjs/cdn-assets](https://github.com/highlightjs/cdn-release) | 11.12.0 | `highlight.min.js` → `highlight.min.js` |
| @highlightjs/cdn-assets | 11.12.0 | `styles/github.min.css` → `highlight-github.min.css` |
| @highlightjs/cdn-assets | 11.12.0 | `styles/github-dark.min.css` → `highlight-github-dark.min.css` |

To update, install reviewed, explicit package versions in a temporary directory
with `npm install --ignore-scripts`, copy the listed files and each package's
`LICENSE`, then update this inventory and run `scripts/smoke-text-shares.cjs`.
The smoke test exercises the actual vendored files, Markdown escaping, blocked
image requests, syntax highlighting, and draft/public views under Crabhop's CSP.
