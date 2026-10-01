import { defineConfig } from 'vite'
import { devtools } from '@tanstack/devtools-vite'
import { nitro } from 'nitro/vite'

import { tanstackStart } from '@tanstack/react-start/plugin/vite'

import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

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
  },
  server: {
    allowedHosts: [
      'ployz-dev.nickpotts.com.au',
      'codex-vm.tailcb9c5.ts.net',
    ],
    strictPort: true,
  },
  plugins: [
    devtools(),
    tailwindcss(),
    tanstackStart({ client: { entry: './client.tsx' } }),
    nitro({
      rollupConfig: { external: [/^@ployz\/sdk(?:\/|$)/] },
      // No page of ours may be framed (clickjacking the Deploy and delete dialogs), always over HTTPS.
      routeRules: {
        '/**': {
          headers: {
            'content-security-policy': "frame-ancestors 'none'",
            'x-frame-options': 'DENY',
            'x-content-type-options': 'nosniff',
            'referrer-policy': 'strict-origin-when-cross-origin',
            'strict-transport-security': 'max-age=31536000',
          },
        },
      },
    }),
    viteReact(),
  ],
})

export default config
