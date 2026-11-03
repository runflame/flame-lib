import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// What the service does with these paths (src/main.rs), so the dev server behaves the same.
const serviceRoutes = {
  name: 'service-routes',
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const path = req.url.split('?')[0];
      if (path === '/classic' || path === '/classic/mint') req.url = '/classic.html';
      else if (path === '/mint') {
        res.writeHead(303, { Location: '/#demo' });
        res.end();
        return;
      }
      next();
    });
  },
};

export default defineConfig({
  plugins: [react(), serviceRoutes],
  appType: 'mpa',
  build: {
    rollupOptions: {
      input: {
        app: fileURLToPath(new URL('index.html', import.meta.url)),
        classic: fileURLToPath(new URL('classic.html', import.meta.url)),
      },
    },
  },
  server: {
    port: 5173,
    strictPort: true,
    // The API comes from a running service: `cargo run --locked` in the crate. The Host header
    // passes through unchanged, so the service's check that writes come from its own origin
    // (Origin against Host) still holds here.
    proxy: { '/api': { target: 'http://127.0.0.1:3000', changeOrigin: false } },
  },
});
