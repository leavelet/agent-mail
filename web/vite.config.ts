import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// `npm run dev` proxies the API to a running `amail serve`
// (AMAIL_DEV_URL, default http://127.0.0.1:18025).
const target = process.env.AMAIL_DEV_URL ?? 'http://127.0.0.1:18025';

export default defineConfig({
  plugins: [svelte()],
  // Built files reference each other relative to the page, so that one
  // build works at the root and under any path prefix (amail serve
  // --base-path, or a reverse proxy's location).
  base: './',
  resolve: process.env.VITEST ? { conditions: ['browser'] } : undefined,
  server: {
    proxy: {
      '/v1': { target, changeOrigin: false },
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
    // No inline scripts or data: modules: the server's CSP is script-src 'self'.
    modulePreload: { polyfill: false },
    assetsInlineLimit: 0,
    sourcemap: false,
  },
  test: {
    environment: 'happy-dom',
    include: ['tests/unit/**/*.test.ts'],
  },
});
