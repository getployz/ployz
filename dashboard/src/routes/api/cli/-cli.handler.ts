import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { approvalsCliRoutes } from "#/modules/approvals/approvals.cli.server";
import { deviceRequestCliRoutes } from "#/modules/devices/device-request.cli.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { tokensCliRoutes } from "#/modules/identity/tokens.cli.server";
import { serversCliRoutes } from "#/modules/machines/servers.cli.server";
import { organizationCliRoutes } from "#/modules/organization/organization.cli.server";
import { passkeysCliRoutes } from "#/modules/passkeys/passkeys.cli.server";
import { proposalsCliRoutes } from "#/modules/proposals/proposals.cli.server";
import { serverUpgradeCliRoutes } from "#/modules/server-upgrade/server-upgrade.cli.server";
import { volumeRunCliRoutes } from "#/modules/volume-run/volume-run.cli.server";
import { mirrorSyncCliRoutes } from "#/modules/volume-copies/mirror-sync/mirror-sync.cli.server";
import { githubCliRoutes } from "#/routes/api/cli/-github.cli";
import type { CliRoutes } from "#/modules/commands/cli-call.server";
import { NotFound } from "#/server/public-error";

/** Every `/api/cli/*` route, each owned by the module whose data it reads or changes. */
const CLI_ROUTES = {
  ...organizationCliRoutes,
  ...tokensCliRoutes,
  ...serversCliRoutes,
  ...serverUpgradeCliRoutes,
  ...approvalsCliRoutes,
  ...proposalsCliRoutes,
  ...deviceRequestCliRoutes,
  ...passkeysCliRoutes,
  ...volumeRunCliRoutes,
  ...mirrorSyncCliRoutes,
  ...githubCliRoutes,
} satisfies CliRoutes;

const isCliRoute = (route: string): route is keyof typeof CLI_ROUTES => Object.hasOwn(CLI_ROUTES, route);

/**
 * `/api/cli/*`: the `ployz` CLI's account surface (Organizations, making and removing them, Organization Tokens, signed-in devices,
 * GitHub connections), and removing, draining, upgrading or cleaning up after a Server Cloud manages through durable runs, or forgetting deleted ones (Forget Servers), reading or deciding an approval a destructive write waits on, and following a Deploy's proposal. Every call acts as one Caller, bound to one Organization. Replies are snake_case JSON for the CLI.
 */
export const cliReply = Effect.fn("Cli.handle")(function* (request: Request) {
  const caller = yield* resolveCaller(request.headers);
  const path = new URL(request.url).pathname.replace(/^\/api\/cli\//, "");
  const [noun, id, ...rest] = path.split("/");
  const route = `${request.method} ${noun}${id === undefined ? "" : "/:id"}${rest.map((segment) => `/${segment}`).join("")}`;
  if (!isCliRoute(route)) return yield* new NotFound({ message: "Not found." });
  const body = yield* CLI_ROUTES[route]({ caller, request, id });
  return body instanceof Response ? body : Response.json(body);
});
