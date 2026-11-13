import { createMDX } from "fumadocs-mdx/next";

const withMDX = createMDX();

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
  // The whole site is static files in `out/`, served by Cloudflare
  // (wrangler.jsonc). There is no server, so images go out as they are.
  output: "export",
  images: { unoptimized: true },
};

export default withMDX(config);
