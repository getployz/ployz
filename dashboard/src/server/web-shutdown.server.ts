import "@tanstack/react-start/server-only";
import { AppRuntime } from "./runtime.server";

export function installWebShutdown() {
  const SHUTDOWN_TIMEOUT_MS = 10_000;

  // Web owns only short requests and browser streams. Deployment execution has
  // its own worker lifecycle; never install this handler in the shared runtime.
  if (import.meta.env.PROD) {
    process.once("SIGTERM", () => {
      const timeout = setTimeout(() => process.exit(1), SHUTDOWN_TIMEOUT_MS);
      void AppRuntime.dispose().then(
        () => {
          clearTimeout(timeout);
          process.exit(0);
        },
        (cause: unknown) => {
          clearTimeout(timeout);
          console.error("Runtime shutdown failed.", cause);
          process.exit(1);
        },
      );
    });
  }
}
