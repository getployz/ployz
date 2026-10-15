"use strict";

class RpcError extends Error {
  constructor({ code, message, details }, options) {
    super(message, options);
    this.name = "RpcError";
    this.code = code;
    this.details = details;
  }
}

const RPC_ERROR_PREFIX = "PLOYZ_RPC_ERROR:";

function throwRpcError(error) {
  if (typeof error?.message !== "string" || !error.message.startsWith(RPC_ERROR_PREFIX)) {
    throw error;
  }
  let payload;
  try {
    payload = JSON.parse(error.message.slice(RPC_ERROR_PREFIX.length));
  } catch {
    throw error;
  }
  if (typeof payload?.code === "string" && typeof payload.message === "string") {
    throw new RpcError(payload, { cause: error });
  }
  throw error;
}

function withRpcError(promise) {
  return promise.catch(throwRpcError);
}

function rpc(fn) {
  return function (...args) {
    let result;
    try { result = fn.apply(this, args); } catch (error) { throwRpcError(error); }
    return result instanceof Promise ? withRpcError(result) : result;
  };
}

function methods(Native) {
  return Object.getOwnPropertyNames(Native.prototype).filter((name) => name !== "constructor");
}

function forward(Target, Native, skip) {
  for (const name of methods(Native)) {
    if (name in Target.prototype || skip.includes(name)) continue;
    Object.defineProperty(Target.prototype, name, {
      value: rpc(function (...args) { return this._inner[name](...args); }),
      writable: true,
      configurable: true,
    });
  }
}

function bindAll(inner, Native) {
  return Object.fromEntries(methods(Native).map((name) => [name, rpc(inner[name].bind(inner))]));
}

module.exports = {
  RpcError,
  throwRpcError,
  withRpcError,
  rpc,
  forward,
  bindAll,
};
