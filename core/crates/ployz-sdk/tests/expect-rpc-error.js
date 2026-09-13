"use strict";

module.exports = function expectRpcError(sdk, error) {
  if (!(error instanceof sdk.RpcError)) throw error;
  return error;
};
