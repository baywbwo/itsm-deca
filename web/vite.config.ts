import { readFileSync } from 'node:fs';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';

// G14: the recorded API responses of the review package (several MB) travel as an inert JSON
// block in review.html instead of inside the JS chunk. The package stays one file for file://.
function reviewFixtures(): Plugin {
  return {
    name: 'itsm-review-fixtures',
    transformIndexHtml() {
      const raw = readFileSync(new URL('./src/review/fixtures.json', import.meta.url), 'utf8');
      // Re-serialised so the block is valid JSON; '<' only occurs inside strings and is escaped
      // so no recorded text can close the script element.
      const children = JSON.stringify(JSON.parse(raw)).replaceAll('<', '\\u003c');
      return [{ tag: 'script', attrs: { type: 'application/json', id: 'itsm-review-fixtures' }, children, injectTo: 'body' }];
    },
  };
}

// The browser only ever talks to its own origin; /api and /health are proxied to the Rust
// API so the session cookie stays first-party (SameSite=Strict). ITSM_API_ORIGIN is read
// here at build/serve time only and is never exposed to the bundle (no VITE_ prefix).
const apiOrigin = process.env.ITSM_API_ORIGIN ?? 'http://127.0.0.1:8080';
const proxy = {
  '/api': { target: apiOrigin, changeOrigin: false },
  '/health': { target: apiOrigin, changeOrigin: false },
};

export default defineConfig(({ mode }) => {
  // Review mode builds a self-contained file:// package that replays recorded API
  // responses; it is never the deployable build.
  const review = mode === 'review';
  return {
    plugins: review ? [react(), reviewFixtures()] : [react()],
    base: review ? './' : '/',
    server: { host: '127.0.0.1', port: 5183, strictPort: true, proxy },
    preview: { host: '127.0.0.1', port: 4183, strictPort: true, proxy },
    build: {
      sourcemap: false,
      assetsInlineLimit: review ? 1024 * 1024 : 4096,
      // Review: one JS chunk (dynamic page imports inlined) so inline-review.mjs can embed it;
      // the deployable build keeps one chunk per page.
      rolldownOptions: review
        ? { input: 'review.html', output: { codeSplitting: false } }
        : { input: 'index.html' },
    },
  };
});
