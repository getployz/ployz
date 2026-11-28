import { openStore, runWorker } from "#/worker/worker.server";
import { exitOnBadConfig } from "#/server/config.server";

exitOnBadConfig();
if (process.argv.includes("--open-store")) {
  try {
    await openStore();
  } catch (error) {
    console.error("Opening the Config Store failed.", error);
    process.exitCode = 1;
  }
} else {
  try {
    await runWorker();
  } catch (error) {
    console.error("Inngest worker failed.", error);
    process.exitCode = 1;
  }
}
