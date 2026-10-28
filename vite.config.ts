import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Tauri expects a fixed dev server port.
export default defineConfig(({ mode }) => ({
  plugins: [svelte()],
  clearScreen: false,
  define: {
    __DEMO__: JSON.stringify(mode === 'demo'),
  },
  server: {
    port: 1420,
    strictPort: true,
    host: '127.0.0.1',
    watch: { ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**'] },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_'],
  build: {
    // WebKitGTK 2.4x is roughly Safari 16+.
    target: 'safari16',
    minify: true,
    sourcemap: false,
    chunkSizeWarningLimit: 2000,
  },
}));
