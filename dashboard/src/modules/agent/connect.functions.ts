import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Schema } from "effect";
import { connectAgent, connectChoices } from "#/modules/agent/connect.server";
import { runAppEffect } from "#/server/run.server";
import { publicErrorMiddleware, strictValidator } from "#/server/tanstack";

/** Who is connecting a remote agent and the Organizations they may connect it to; null when signed out. */
export const connectChoicesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware])
  .handler(() => {
    const request = getRequest();
    return runAppEffect(connectChoices(request.headers), { signal: request.signal });
  });

/** Grants or refuses the authorization request the page was opened with, bound to the chosen Organization. */
export const connectAgentServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationId: Schema.String, accept: Schema.Boolean, oauthQuery: Schema.String })))
  .handler(({ data }) => {
    const request = getRequest();
    return runAppEffect(connectAgent(request.headers, data), { signal: request.signal });
  });
