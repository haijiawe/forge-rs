import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';

// vite.config.ts runs in Node, but @types/node is deliberately not a dependency
// (the dependency list is pinned by the task), so env access is typed here.
const nodeEnv = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process
  ?.env;

// The UI is served by `forge serve` from web/dist under '/', and the dev server
// proxies /api to the Go server on 127.0.0.1:8848 (override with
// FORGE_DEV_API, e.g. FORGE_DEV_API=http://127.0.0.1:9000).
const devApiTarget = nodeEnv?.['FORGE_DEV_API'] ?? 'http://127.0.0.1:8848';

export default defineConfig({
  base: '/',
  plugins: [react()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // Source maps stay out of the binary (docs/02-architecture.md §10.1.7).
    sourcemap: false,
    target: 'es2022',
    reportCompressedSize: true,
  },
  server: {
    proxy: {
      '/api': { target: devApiTarget, ws: true, changeOrigin: false },
      '/healthz': { target: devApiTarget, changeOrigin: false },
    },
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.{ts,tsx}'],
    globals: false,
    restoreMocks: true,
    // The OS sandbox blocks `fork()` (vitest's default "forks" pool dies with
    // EPERM), so tests run in worker threads instead. This is an environment
    // constraint, not a test-design choice; the suite passes either way.
    pool: 'threads',
    fileParallelism: false,
  },
});