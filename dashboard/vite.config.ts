import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { defineConfig, type Plugin } from 'vite'
import { devtools } from '@tanstack/devtools-vite'
import { nitro } from 'nitro/vite'

import { tanstackStart } from '@tanstack/react-start/plugin/vite'

import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

const marketingOrigin = process.env['MARKETING_ORIGIN']

// Dev only: `/__login` signs a browser in as the verify seed's user, for opening a verify env from another device.
const verifyLogin: Plugin = {
  name: 'verify-login',
  apply: 'serve',
  configureServer(server) {
    server.middlewares.use('/__login', (_req, res) => {
      const seed = join(server.config.root, '.verify/run/seed.json')
      if (!existsSync(seed)) {
        res.statusCode = 404
        res.end('No verify seed. Run scripts/verify/up.sh.')
        return
      }
      const { cookie, organizationSlug } = JSON.parse(readFileSync(seed, 'utf8'))
      const attrs = 'Path=/; HttpOnly; SameSite=Lax; Max-Age=604800'
      res.setHeader('Set-Cookie', [
        `better-auth.session_token=${cookie}; ${attrs}`,
        `__Secure-better-auth.session_token=${cookie}; ${attrs}; Secure`,
      ])
      res.statusCode = 302
      res.setHeader('Location', `/cloud/${organizationSlug}/shop/production`)
      res.end()
    })
  },
}

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
    // Dev only: Vite answers asset-looking paths (`.js`, `.css`, images) itself before any app route runs, so the
    // marketing proxy (src/routes/$.tsx) never sees the marketing site's built assets. Production has no such layer.
    proxy: marketingOrigin ? { '/_astro': marketingOrigin, '/pagefind': marketingOrigin } : undefined,
  },
  plugins: [
    verifyLogin,
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
