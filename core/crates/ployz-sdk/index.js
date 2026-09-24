"use strict";

// ponytail: the specifier is computed so bundlers cannot follow require into the
// .node binary. Nitro/Vinxi emit this file as ESM without CJS module globals.
const native = require([".", "ployz-sdk.node"].join("/"));

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

const runtimeLogs = require("./runtime-logs.js");

class Client {
  constructor(inner) {
    this._inner = inner;
    const logTransport = {
      watch: (options) => iterateWatch(() => inner.watch(), options.signal),
      open: async (input) => {
        const reader = await withRpcError(inner.containerLogs(input));
        return { next: () => withRpcError(reader.next()), cancel: () => reader.cancel() };
      },
    };
    this.runtime = {
      watch: (options = {}) => iterateWatch(() => inner.watch(), options && options.signal),
      logs: (options = {}) => runtimeLogs.logs(logTransport, options),
      logHistory: (options) => runtimeLogs.history(logTransport, options),
    };
  }

  observeEnrollment() {
    return withRpcError(this._inner.observeEnrollment());
  }

  register(assignment) {
    return withRpcError(this._inner.register(assignment));
  }

  clearManagementClient(label) {
    return withRpcError(this._inner.clearManagementClient(label));
  }

  inspect() {
    return withRpcError(this._inner.inspect());
  }

  about() {
    return withRpcError(this._inner.about());
  }

  prepare(input, options = {}) {
    options.signal?.throwIfAborted();
    let pending;
    try { pending = this._inner.prepare(input); } catch (error) { throwRpcError(error); }
    const stop = () => pending.abort();
    options.signal?.addEventListener("abort", stop, { once: true });
    const finished = withRpcError(pending.finished()).then(wrapPreview)
      .finally(() => options.signal?.removeEventListener("abort", stop));
    // Callers may consume progress before awaiting the terminal result.
    void finished.catch(() => {});
    return {
      abort: stop,
      finished,
      async *[Symbol.asyncIterator]() {
        for (;;) {
          const value = await withRpcError(pending.next());
          if (value == null) return;
          yield value;
        }
      },
    };
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

  pruneImages(targets) {
    return withRpcError(this._inner.pruneImages(targets));
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
    buildReceipts: handle.buildReceipts(),
    pruneTargets: handle.pruneTargets(),
    close: () => handle.close(),
    confirm(options = {}) {
      try {
        return wrapRunning(handle.confirm(options?.deploymentId, options?.imageCleanup), options && options.signal);
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
      finished ??= withRpcError(running.finished());
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
  const { signal, timeoutMs } = options;
  if (timeoutMs !== undefined && (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 2147483647)) {
    throw new TypeError("timeoutMs must be a positive 32-bit integer");
  }
  signal?.throwIfAborted();
  let attempt;
  try { attempt = native.startConnections(options.connections); } catch (error) { throwRpcError(error); }
  let client;
  let timer;
  let stopped = false;
  const stop = () => { stopped = true; attempt.cancel(); void client?.close(); };
  const cleanup = () => { clearTimeout(timer); signal?.removeEventListener("abort", stop); };
  signal?.addEventListener("abort", stop, { once: true });
  if (timeoutMs !== undefined) timer = setTimeout(stop, timeoutMs);
  try {
    client = new Client(await withRpcError(attempt.wait()));
    const close = client.close.bind(client);
    client.close = () => { cleanup(); return close(); };
    if (stopped) { await client.close(); signal?.throwIfAborted(); throw new Error("session deadline exceeded"); }
    return client;
  } catch (error) { cleanup(); throw error; }
}

module.exports = {
  allocateEnrollment: (...args) => {
    try { return native.allocateEnrollment(...args); } catch (error) { throwRpcError(error); }
  },
  connect,
  Client,
  RpcError,
  applyAll,
  applyOne,
};
