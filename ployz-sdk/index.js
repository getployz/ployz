"use strict";

// ponytail: specifiers are computed so bundlers cannot follow require into the
// .node binary. Nitro/Vinxi emit this file as ESM without CJS module globals.
const localBinding = [".", "ployz-sdk.node"].join("/"); // scripts/check-sdk-package.sh, tests
const bindingPackage = `@ployz/sdk-${process.platform}-${process.arch}`;
let native;
for (const specifier of [localBinding, bindingPackage]) {
  try {
    native = require(specifier);
    break;
  } catch (error) {
    if (error.code !== "MODULE_NOT_FOUND") {
      throw error;
    }
  }
}
if (!native) {
  throw new Error(
    `@ployz/sdk has no native binding for ${process.platform}-${process.arch}: install ${bindingPackage}`,
  );
}

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

class Client {
  constructor(inner) {
    this._inner = inner;
    this.runtime = {
      watch: (options = {}) => iterateWatch(() => inner.watch(), options && options.signal),
    };
  }

  about() {
    return withRpcError(this._inner.about());
  }

  async preview(intent) {
    return wrapPreview(await withRpcError(this._inner.preview(intent)));
  }

  async previewProjectRemoval(projectName, destroyVolumes) {
    return wrapPreview(
      await withRpcError(this._inner.previewProjectRemoval(projectName, destroyVolumes)),
    );
  }

  async run(intent, options = {}) {
    const preview = await this.preview(intent);
    const running = preview.confirm(options);
    return running.finished;
  }

  removeVolumes(request) {
    return withRpcError(this._inner.removeVolumes(request));
  }

  dataLossIfMachineRemoved(machine) {
    return withRpcError(this._inner.dataLossIfMachineRemoved(machine));
  }

  removeMachine(machine, confirmDataLoss) {
    return withRpcError(this._inner.removeMachine(machine, confirmDataLoss));
  }

  dataLossIfProjectDestroyed(projectName, destroyVolumes = false) {
    return withRpcError(this._inner.dataLossIfProjectDestroyed(projectName, destroyVolumes));
  }

  destroyProject(projectName, confirmDataLoss, destroyVolumes = false) {
    return withRpcError(this._inner.destroyProject(projectName, confirmDataLoss, destroyVolumes));
  }

  dataLossIfClusterDestroyed() {
    return withRpcError(this._inner.dataLossIfClusterDestroyed());
  }

  destroyCluster(confirmDataLoss) {
    return withRpcError(this._inner.destroyCluster(confirmDataLoss));
  }

  close() {
    return this._inner.close();
  }
}

function wrapPreview(handle) {
  const payload = handle.payload();
  return {
    ...payload,
    noop: payload.operations.length === 0,
    confirm(options = {}) {
      try {
        return wrapRunning(handle.confirm(), options && options.signal);
      } catch (error) {
        throwRpcError(error);
      }
    },
  };
}

function wrapRunning(running, signal) {
  const stop = () => running.abort();
  if (signal?.aborted) {
    stop();
  } else if (signal) {
    signal.addEventListener("abort", stop, { once: true });
  }
  let finished;
  return {
    abort: stop,
    get finished() {
      finished ??= running.finished();
      return finished;
    },
    async *[Symbol.asyncIterator]() {
      try {
        for (;;) {
          const value = await running.next();
          if (value == null) {
            return;
          }
          yield value;
        }
      } finally {
        if (signal) {
          signal.removeEventListener("abort", stop);
        }
      }
    },
  };
}

async function* iterateWatch(start, signal) {
  if (signal?.aborted) {
    return;
  }
  let stream;
  const stop = () => {
    if (stream) {
      stream.cancel();
    }
  };
  if (signal) {
    signal.addEventListener("abort", stop, { once: true });
  }
  try {
    stream = await withRpcError(start());
    if (signal?.aborted) {
      stream.cancel();
      return;
    }
    for (;;) {
      const value = await withRpcError(stream.next());
      if (value == null || signal?.aborted) {
        return;
      }
      yield value;
    }
  } finally {
    if (signal) {
      signal.removeEventListener("abort", stop);
    }
    stop();
  }
}

function defaultPlanOptions() {
  return {
    force_recreate: false,
    skip_health_monitor: false,
    placement_seed: 0,
    selected: [],
  };
}

function applyAll(project_name, specs, options = defaultPlanOptions()) {
  return {
    project_name,
    target: specs,
    options,
  };
}

function applyOne(project_name, spec, options = defaultPlanOptions()) {
  return {
    project_name,
    target: [spec],
    options: {
      ...defaultPlanOptions(),
      ...options,
      selected: [{ name: spec.name }],
    },
  };
}

async function connect(options) {
  return new Client(await withRpcError(native.connect(options)));
}

module.exports = {
  connect,
  listHeld: (...args) => withRpcError(native.listHeld(...args)),
  register: (...args) => withRpcError(native.register(...args)),
  revokePairing: (...args) => withRpcError(native.revokePairing(...args)),
  packageName: native.packageName,
  Client,
  RpcError,
  applyAll,
  applyOne,
};
