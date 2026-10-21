import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { requirements, serversLacking } from "#/modules/runtime/daemon-compat";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";

type WatchedMachine = RuntimeWatchView["machines"][number]["machine"];

/** The Servers `include` picks from `frame`, every one by default, whose records say they can't run EnsureContainer. */
export const lackingEnsure = (frame: RuntimeWatchView | null, include: (machine: WatchedMachine) => boolean = () => true) =>
  serversLacking(
    (frame?.machines ?? [])
      .filter(({ machine }) => include(machine))
      .map(({ machine }) => ({ name: machine.name, capabilities: machine.runtime.capabilities ?? [] })),
    requirements.ensureContainer,
  );

/**
 * The Servers whose records say they can't run EnsureContainer. A Deploy's plan can create or remove a Container on any
 * Server, and Cloud only reads it as it runs, so a Deploy counts them all. A Server Cloud can't read is attempted, as
 * are all of them when there is no frame.
 */
export const serversLackingEnsure = (organizationId: string) => firstRuntimeFrame(organizationId).pipe(
  Effect.map((frame) => lackingEnsure(frame)),
  Effect.orElseSucceed((): string[] => []),
);
