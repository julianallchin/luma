# www

The Luma website and documentation, served at [luma.show](https://luma.show). It
is a Next.js app with Fumadocs, exported as static files.

- Pages and layouts: `src/app/` (`(marketing)` and `(docs)`).
- Documentation content: `content/docs/`.
- Icons: `src/app/favicon.ico`, `icon.png` and `apple-icon.png`, cut from the
  app's own icon (`assets/app/icon.png`).

Use Bun:

```sh
cd www
bun install
bun run dev            # development server on http://localhost:3000
bun run build          # the static site, in out/
bunx wrangler dev      # out/ as Cloudflare serves it
```

## Hosting

The site is static files on Cloudflare (`wrangler.jsonc`): no server, no image
optimizer, no search. Cloudflare builds it from the repository with root
directory `www`, build command `bun run build` and deploy command
`npx wrangler deploy`. `public/_redirects` keeps the links of removed docs
pages working; installed copies of the app still open one from Help.
