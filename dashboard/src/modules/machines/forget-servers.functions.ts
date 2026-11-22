import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { ForgetServersInput } from "./machine-removal";
import { checkForgetServers, forgetServers, forgetterFor } from "./forget-servers.server";

const input = strictValidator(ForgetServersInput);

/** Forget Servers' check as its dialog opens: every Server tried; refused (as data) when one answers. */
export const previewForgetServersServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(input)
  .handler(({ context, data }) => runActor(context, forgetterFor(context.actor, data.organizationSlug).pipe(
    Effect.flatMap(checkForgetServers),
  )));

/** Forget Servers: tries every Server again, and forgets the Cluster only when none answers. */
export const forgetServersServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(input)
  .handler(({ context, data }) => runActor(context, forgetterFor(context.actor, data.organizationSlug).pipe(
    Effect.flatMap(forgetServers),
  )));
