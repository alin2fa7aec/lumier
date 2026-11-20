import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const backend = process.env.lumier_BACKEND ?? 'http://127.0.0.1:8080'

export default defineConfig({
  plugins: [react()],
  server: {
    // Proxying keeps the dev setup same-origin, so no CORS handling is needed
    // on the Rust side.
    proxy: {
      '/api': backend,
      '/img': backend,
    },
  },
})
