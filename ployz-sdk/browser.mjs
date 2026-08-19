// Vite and TanStack Start walk server-fn deps with browser export conditions.
// This file must not import node: builtins or the native binding.
function fail() {
  throw new Error(
    "@ployz/sdk only runs on Node.js. Import it from server code, not the browser.",
  );
}

export const connect = fail;
export const listHeld = fail;
export const register = fail;
export const revokePairing = fail;
export const applyAll = fail;
export const applyOne = fail;
export const packageName = fail;
export const Client = fail;
