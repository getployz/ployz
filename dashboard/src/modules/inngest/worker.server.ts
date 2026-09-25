import { Data, Effect } from "effect";
import { createServer } from "node:http";
import { connect, ConnectionState, type WorkerConnection } from "inngest/connect";
import { InngestClient, type PloyzInngest } from "#/modules/inngest/client";
import { createInngestFunctions } from "#/modules/inngest/index";
import { billingInngestFunctions } from "#/modules/billing/inngest-sync/sync";
import { AppConfig, type PolarConfiguration } from "#/server/config.server";
import { AppRuntime } from "#/server/runtime.server";

class InngestConnectionError extends Data.TaggedError("InngestConnectionError")<{
  readonly cause: unknown;
}> {}

const connectWorker = Effect.fn("Inngest.connectWorker")((
  client: PloyzInngest,
  billingMode: PolarConfiguration["mode"],
) =>
  Effect.tryPromise({
    try: () => connect({
      apps: [{
        client,
        functions: [...createInngestFunctions(client), ...billingInngestFunctions(client, billingMode)],
      }],
    }),
    catch: (cause) => new InngestConnectionError({ cause }),
  }),
);

/** Connect owns draining; application resources must outlive all active steps. */
export async function runWorker() {
  let connection: WorkerConnection | undefined;
  const health = createServer((request, response) => {
    const status = request.url !== "/ready" ? 404
      : connection?.state === ConnectionState.ACTIVE ? 200 : 503;
    response.writeHead(status).end();
  });
  try {
    const config = await AppRuntime.runPromise(AppConfig);
    await new Promise<void>((resolve, reject) => {
      health.once("error", reject);
      health.listen(config.app.port, "0.0.0.0", resolve);
    });
    const client = await AppRuntime.runPromise(InngestClient);
    connection = await AppRuntime.runPromise(connectWorker(client, config.polar.mode));
    console.info("Inngest worker ready.");
    await connection.closed;
    console.info("Inngest worker drained.");
  } finally {
    health.closeAllConnections();
    await new Promise<void>((resolve) => health.close(() => resolve()));
    await AppRuntime.dispose();
  }
}
