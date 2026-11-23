import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const backend = process.env.LUMIER_BACKEND ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    // Proxying keeps the dev setup same-origin, so no CORS handling is needed
    // on the Rust side. The Host header is passed through untouched: the
    // server refuses a change whose Origin does not match its Host, and the
    // string shorthand would rewrite Host to the backend's address.
    proxy: {
      '/api': { target: backend, changeOrigin: false },
      '/img': { target: backend, changeOrigin: false },
    },
  },
})
