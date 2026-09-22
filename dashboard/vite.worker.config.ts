import { defineConfig } from "vite";

export default defineConfig({
  resolve: { tsconfigPaths: true },
  // Keep Inngest's worker-thread runner beside its installed package.
  ssr: { external: ["inngest", "inngest/connect", "@ployz/sdk"] },
  build: {
    ssr: "src/worker.ts",
    outDir: ".output/worker",
    target: "node24",
    rollupOptions: { output: { entryFileNames: "index.mjs" } },
  },
});
