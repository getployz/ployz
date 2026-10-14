// Vite and TanStack Start walk server-fn deps with browser export conditions.
// This file must not import node: builtins or the native binding.
function fail() {
  throw new Error(
    "@ployz/sdk only runs on Node.js. Import it from server code, not the browser.",
  );
}

export const connect = fail;
export const applyAll = fail;
export const applyOne = fail;
export const Client = fail;
export const RpcError = fail;

export const allocateEnrollment = fail;
export const ployzVersion = fail;
export const agentTools = fail;
export const agentStart = fail;
export const agentResume = fail;
export const agentCloudCall = fail;
export const openConfigStore = fail;
export const observeVolumes = fail;
export const observeCopies = fail;
