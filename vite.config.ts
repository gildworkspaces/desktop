import { defineConfig } from 'vite'
import vinext from 'vinext'

// Static files only (next.config.ts: output 'export'): Tauri serves the build
// from disk and there is no server behind the window. Everything the app
// stores goes through the Rust core.
export default defineConfig({
  plugins: [vinext()],
})
