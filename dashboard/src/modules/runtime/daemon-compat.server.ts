import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { requirements, serversLacking } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";

/**
 * The Servers `include` picks, every one by default, whose records say they can't run EnsureContainer. A Deploy's plan
 * can create or remove a Container on any Server, and Cloud only reads it as it runs, so a Deploy counts them all. A
 * Server Cloud can't read is attempted, as are all of them when there is no frame.
 */
export const serversLackingEnsure = (
  organizationId: string,
  include: (machine: RuntimeWatchView["machines"][number]["machine"]) => boolean = () => true,
) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => serversLacking(
    (frame?.machines ?? [])
      .filter(({ machine }) => include(machine))
      .map(({ machine }) => ({ name: machine.name, capabilities: machine.runtime.capabilities ?? [] })),
    requirements.ensureContainer,
  )),
  Effect.orElseSucceed((): string[] => []),
);
