import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // Relative URLs throughout: standalone hora serves the page at `/`, and a
  // Tracon node serves it under `/t/<token>/`.
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  server: { proxy: { '/api': 'http://127.0.0.1:4530' } }
});
