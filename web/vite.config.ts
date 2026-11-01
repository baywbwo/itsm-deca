import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

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
    plugins: [react()],
    base: review ? './' : '/',
    server: { host: '127.0.0.1', port: 5183, strictPort: true, proxy },
    preview: { host: '127.0.0.1', port: 4183, strictPort: true, proxy },
    build: {
      sourcemap: false,
      assetsInlineLimit: review ? 1024 * 1024 : 4096,
      rolldownOptions: review
        ? { input: 'review.html' }
        : { input: 'index.html' },
    },
  };
});
