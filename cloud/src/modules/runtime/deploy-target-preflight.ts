import type { RuntimeLensStatus } from "#/modules/runtime/runtime.collection";

export type DeployTargetPreflight =
  | { ok: true }
  | {
      ok: false;
      title: string;
      description: string;
      action: "add_server" | null;
    };

export function getDeployTargetPreflight(input: {
  status: RuntimeLensStatus;
  machineCount: number;
  isLoading: boolean;
  error: string | null;
}): DeployTargetPreflight {
  if (!input.isLoading && input.status === "observed" && input.machineCount > 0) {
    return { ok: true };
  }

  if (input.isLoading || input.status === "connecting") {
    return {
      ok: false,
      title: "Checking servers",
      description: "Cloud is checking whether this project has a server to deploy to.",
      action: null,
    };
  }

  if (input.status === "unavailable" || input.status === "unreachable") {
    return {
      ok: false,
      title: "Runtime unavailable",
      description: input.error ?? "Cloud could not reach the connected runtime.",
      action: null,
    };
  }

  return {
    ok: false,
    title: "Add a machine before deploying",
    description:
      "You can keep configuring this project, then add a machine when you are ready to run it.",
    action: "add_server",
  };
}
