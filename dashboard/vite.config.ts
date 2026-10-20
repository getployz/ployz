import { defineConfig } from 'vite'
import { devtools } from '@tanstack/devtools-vite'
import { nitro } from 'nitro/vite'

import { tanstackStart } from '@tanstack/react-start/plugin/vite'

import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { SECURITY_HEADERS } from './src/server/security-headers'

const marketingOrigin = process.env['MARKETING_ORIGIN']

const config = defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  test: {
    setupFiles: ["./vitest.setup.ts"],
  },
  // ponytail: native NAPI addon, rolldown can't parse the .node binary
  ssr: {
    external: ['@ployz/sdk'],
  },
  optimizeDeps: {
    exclude: ['@ployz/sdk'],
    // tanstackStart excludes better-auth, so the scanner never sees what its client imports; vite then finds these
    // on first page load, re-optimizes and reloads into stale `?v=` hashes (504 Outdated Optimize Dep).
    include: [
      '@better-auth/core',
      '@better-auth/core/api',
      '@better-auth/core/db',
      '@better-auth/core/db/adapter',
      '@better-auth/core/db/internal',
      '@better-auth/core/env',
      '@better-auth/core/error',
      '@better-auth/core/instrumentation',
      '@better-auth/core/oauth2',
      '@better-auth/core/social-providers',
      '@better-auth/core/utils/db',
      '@better-auth/core/utils/deprecate',
      '@better-auth/core/utils/email',
      '@better-auth/core/utils/error-codes',
      '@better-auth/core/utils/host',
      '@better-auth/core/utils/id',
      '@better-auth/core/utils/ip',
      '@better-auth/core/utils/is-api-error',
      '@better-auth/core/utils/json',
      '@better-auth/core/utils/string',
      '@better-auth/core/utils/url',
      'nanostores',
    ],
  },
  server: {
    allowedHosts: [
      'ployz-dev.nickpotts.com.au',
      'codex-vm.tailcb9c5.ts.net',
    ],
    strictPort: true,
    // Dev only: Vite answers asset-looking paths (`.js`, `.css`, images) itself before any app route runs, so the
    // marketing proxy (src/routes/$.tsx) never sees the marketing site's built assets. Production has no such layer.
    proxy: marketingOrigin ? { '/_astro': marketingOrigin, '/pagefind': marketingOrigin } : undefined,
  },
  plugins: [
    devtools(),
    tailwindcss(),
    tanstackStart({ client: { entry: './client.tsx' } }),
    nitro({
      rollupConfig: { external: [/^@ployz\/sdk(?:\/|$)/] },
      routeRules: { '/**': { headers: { ...SECURITY_HEADERS } } },
    }),
    viteReact(),
  ],
})

export default config
