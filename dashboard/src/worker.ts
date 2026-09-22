import { runWorker } from "#/modules/inngest/worker.server";

try {
  await runWorker();
} catch (error) {
  console.error("Inngest worker failed.", error);
  process.exitCode = 1;
}
