import type { NextConfig } from 'next'

// Static files only: Tauri serves the exported build and there is no server
// behind the window. Everything the app stores goes through the Rust core.
const nextConfig: NextConfig = {
  output: 'export',
  images: { unoptimized: true },
  reactStrictMode: true,
}

export default nextConfig
