import { runWorker } from "#/worker/worker.server";
import { exitOnBadConfig } from "#/server/config.server";

exitOnBadConfig();
try {
  await runWorker();
} catch (error) {
  console.error("Inngest worker failed.", error);
  process.exitCode = 1;
}
