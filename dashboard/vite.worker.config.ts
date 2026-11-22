import { defineConfig } from "vite";
import { PLOYZ_SDK } from "./vite.sdk";

export default defineConfig({
  resolve: { tsconfigPaths: true },
  // Keep Inngest's worker-thread runner beside its installed package.
  ssr: { external: ["inngest", "inngest/connect", PLOYZ_SDK] },
  build: {
    ssr: "src/worker.ts",
    outDir: ".output/worker",
    target: "node24",
    rollupOptions: { output: { entryFileNames: "index.mjs" } },
  },
});
