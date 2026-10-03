// Loads a TS module through this checkout's Vite (so `#/` imports, TS and the SDK resolve like the app's) and calls its
// `run()`. Run from dashboard/: node scripts/verify/runner.mjs /scripts/verify/seed.ts
import { createServer } from "vite";

const server = await createServer({
  configFile: false, root: process.cwd(), logLevel: "warn", appType: "custom",
  server: { middlewareMode: true, hmr: false, ws: false },
  resolve: { tsconfigPaths: true },
  ssr: { external: ["@ployz/sdk"] },
  optimizeDeps: { noDiscovery: true, include: [] },
});
try {
  await (await server.ssrLoadModule(process.argv[2])).run();
} finally {
  await server.close();
}
process.exit(0);
