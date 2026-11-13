# www

The Luma website and documentation, served at [luma.show](https://luma.show). It
is a Next.js app with Fumadocs.

- Pages and layouts: `src/app/` (`(marketing)` and `(docs)`).
- Documentation content: `content/docs/`.
- Icons: `src/app/favicon.ico`, `icon.png` and `apple-icon.png`, cut from the
  app's own icon (`assets/app/icon.png`).

Use Bun:

```sh
cd www
bun install
bun run dev     # development server on http://localhost:3000
bun run build   # production build
```
