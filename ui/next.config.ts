import type { NextConfig } from "next";

/**
 * Static export is mandatory for Tauri.
 *
 * `make ui` overrides `distDir` to `out-dev` so the fast development server
 * does not overwrite the production bundle `make run` produced, which Tauri
 * points at as `frontendDist`.
 */
const nextConfig: NextConfig = {
  output: "export",
  distDir: process.env.NEXT_PUBLIC_DEV_OUT ? "out-dev" : "out",
  // The static export has no server to run the image optimiser.
  images: { unoptimized: true },
};

export default nextConfig;
