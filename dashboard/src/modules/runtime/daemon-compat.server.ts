import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { frameServers, requirements, serversLacking, type Requirement } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";

/** The Servers `include` picks from `frame`, every one by default, whose records say they can't run EnsureContainer. */
export const lackingEnsure = (frame: RuntimeWatchView | null, include?: (machine: RuntimeWatchView["machines"][number]["machine"]) => boolean) =>
  serversLacking(frameServers(frame, include), requirements.ensureContainer);

/** The Servers whose records say they lack `requirement`. No answer names none: the call is attempted. */
export const serversLackingIn = (organizationId: string, requirement: Requirement) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => serversLacking(frameServers(frame), requirement)),
  Effect.orElseSucceed((): string[] => []),
);

/**
 * The Servers whose records say they can't run EnsureContainer. A Deploy's plan can create or remove a Container on any
 * Server, and Cloud only reads it as it runs, so a Deploy counts them all. A Server Cloud can't read is attempted, as
 * are all of them when there is no frame.
 */
export const serversLackingEnsure = (organizationId: string) => serversLackingIn(organizationId, requirements.ensureContainer);
