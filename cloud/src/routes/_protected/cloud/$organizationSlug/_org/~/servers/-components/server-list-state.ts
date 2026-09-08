import {
  CLUSTER_UNREACHABLE_ERROR,
  type RuntimeIncompleteIds,
  type RuntimeLensStatus,
} from "#/modules/runtime/runtime.collection";

type ConnectionNotice = {
  title: string;
  description: string;
};

/** Incomplete IDs are explicitly not deletions. Keep that uncertainty visible
 * beside the direct container counts shown by the Server list. */
export function incompleteRuntimeObservationDescription(
  input: RuntimeIncompleteIds,
) {
  const parts = [
    input.machines.length > 0
      ? `${input.machines.length} ${input.machines.length === 1 ? "machine" : "machines"}`
      : null,
    input.containers.length > 0
      ? `${input.containers.length} ${input.containers.length === 1 ? "container" : "containers"}`
      : null,
    input.volumes.length > 0
      ? `${input.volumes.length} ${input.volumes.length === 1 ? "volume" : "volumes"}`
      : null,
    input.certificates.length > 0
      ? `${input.certificates.length} ${input.certificates.length === 1 ? "certificate" : "certificates"}`
      : null,
  ].filter((part): part is string => part !== null);

  return parts.length > 0
    ? `The Runtime Watch lists incomplete IDs for ${parts.join(", ")}. Server workload counts include only containers it observed.`
    : null;
}

type ServerListState =
  | {
      kind: "rows";
      notice?: ConnectionNotice;
    }
  | { kind: "loading"; notice?: never }
  | {
      kind: "empty";
      title: string;
      description: string;
      variant: "first-run" | "no-results" | "placeholder";
      notice?: ConnectionNotice;
    };

export function getServerListState(input: {
  rowCount: number;
  visibleRowCount: number;
  query: string;
  runtimeStatus: RuntimeLensStatus;
  runtimeError: string | null;
}): ServerListState {
  const showingRuntimeMachines = input.visibleRowCount > 0;
  let notice: ConnectionNotice | undefined;
  switch (input.runtimeStatus) {
    case "no_connection":
      notice = {
        title: "No Cloud Connection",
        description: "Add a server to create a cluster and connect it to Cloud",
      };
      break;
    case "unavailable":
      notice = {
        title: showingRuntimeMachines
          ? "Showing the last observed runtime state"
          : "Runtime unavailable",
        description:
          input.runtimeError ??
          (showingRuntimeMachines
            ? "Runtime is unreachable. These servers reflect the last update before the connection was lost."
            : "Cloud couldn't connect to the cluster"),
      };
      break;
    case "unreachable":
      notice = {
        title: "Can't reach the cluster",
        description:
          input.runtimeError ?? CLUSTER_UNREACHABLE_ERROR,
      };
      break;
    case "connecting":
    case "observed":
      notice = undefined;
      break;
    default: {
      const _exhaustive: never = input.runtimeStatus;
      return _exhaustive;
    }
  }

  if (input.visibleRowCount > 0) {
    return notice ? { kind: "rows", notice } : { kind: "rows" };
  }

  if (input.runtimeStatus === "connecting") return { kind: "loading" };

  if (input.rowCount > 0 && input.query.trim()) {
    return notice
      ? {
          kind: "empty",
          title: "No matches",
          description: "Try another search",
          variant: "no-results",
          notice,
        }
      : {
          kind: "empty",
          title: "No matches",
          description: "Try another search",
          variant: "no-results",
        };
  }

  if (notice) {
    return {
      kind: "empty",
      title: notice.title,
      description: notice.description,
      variant:
        input.runtimeStatus === "no_connection" ? "first-run" : "placeholder",
    };
  }

  return {
    kind: "empty",
    title: "No servers in the latest observation",
    description:
      "The Runtime Watch entry returned no server observations",
    variant: "placeholder",
  };
}
