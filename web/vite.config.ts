import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// In development the app runs on :5173 and passes API calls to the server on
// :7430, so the session cookie is same-origin as it is in production, where
// the server serves the built app itself (PAYVY_WEB_DIR).
const api = process.env.PAYVY_API ?? 'http://127.0.0.1:7430';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      '/api': api,
      '/auth': api,
      '/mcp': api,
      '/health': api,
    },
  },
  build: { sourcemap: true },
});
