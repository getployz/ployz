import type { ReactNode } from "react";

/** How Sealed shows: offered unless no server disables it beside `addServer`, a link to add one. */
export type SealedControls = {
  state: "no_server" | "ready" | undefined;
  addServer: ReactNode;
};

export type VariableAddInput = {
  key: string;
  value: string;
  sealed: boolean;
  exported: boolean;
};
