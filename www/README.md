# www

The Luma website and documentation, served at [luma.show](https://luma.show). It
is a Next.js app with Fumadocs.

- Pages and layouts: `src/app/` (`(marketing)` and `(docs)`).
- Documentation content: `content/docs/`.

Use Bun:

```sh
cd www
bun install
bun run dev     # development server on http://localhost:3000
bun run build   # production build
```
