import {
  CLUSTER_UNREACHABLE_ERROR,
  type RuntimeLensStatus,
} from "#/modules/runtime/runtime.collection";

type ConnectionNotice = {
  title: string;
  description: string;
};

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
    case "live_empty":
    case "live_rows":
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
    title: "No active servers",
    description:
      "Cloud is connected, but the cluster returned no active servers",
    variant: "placeholder",
  };
}
