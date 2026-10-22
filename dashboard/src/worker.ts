import { runWorker } from "#/modules/inngest/worker.server";
import { startupConfigFailure } from "#/server/config.server";

const configFailure = startupConfigFailure();
if (configFailure !== undefined) {
  console.error(configFailure);
  process.exitCode = 1;
} else {
  try {
    await runWorker();
  } catch (error) {
    console.error("Inngest worker failed.", error);
    process.exitCode = 1;
  }
}
