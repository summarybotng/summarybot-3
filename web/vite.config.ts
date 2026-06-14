import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Dev proxy: forward API + SSE paths to the Rust server on :8080 so the SPA and
// API share an origin (no CORS) during development. In production the same
// server serves the built assets, so no proxy is needed.
const target = 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      '/auth': target,
      '/workspaces': target,
      '/tenants': target,
      '/tenant': target,
      '/operator': target,
      '/oauth': target,
      '/invites': target,
      '/healthz': target,
      '/openapi.json': target,
    },
  },
  build: { outDir: 'dist' },
})
