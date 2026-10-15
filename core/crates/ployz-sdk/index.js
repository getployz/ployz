"use strict";

// ponytail: the specifier is computed so bundlers cannot follow require into the
// .node binary. Nitro/Vinxi emit this file as ESM without CJS module globals.
const native = require([".", "ployz-sdk.node"].join("/"));

const { RpcError, throwRpcError, withRpcError, rpc, forward, bindAll } = require("./wrap.js");
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
      history: async (input) => {
        const reader = await withRpcError(inner.logHistory(input));
        return { next: () => withRpcError(reader.next()), cancel: () => reader.cancel() };
      },
    };
    this.runtime = {
      watch: (options = {}) => iterateWatch(() => inner.watch(), options && options.signal),
      logs: (options = {}) => runtimeLogs.logs(logTransport, options),
      logHistory: (options) => runtimeLogs.history(logTransport, options),
      metricHistory: (input) => withRpcError(inner.metricHistory(input)),
    };
  }

  prepare(input, options = {}) {
    return wrapProgress(() => this._inner.prepare(input), options.signal, wrapPreview);
  }

  build(input, options = {}) {
    return wrapProgress(() => this._inner.build(input, options.startWithinMs), options.signal);
  }

  async preview(intent) {
    return wrapPreview(await withRpcError(this._inner.preview(intent)));
  }

  async previewNamespaceRemoval(namespace, destroyVolumes) {
    return wrapPreview(
      await withRpcError(this._inner.previewNamespaceRemoval(namespace, destroyVolumes)),
    );
  }

  async run(intent, options = {}) {
    const preview = await this.preview(intent);
    const running = preview.confirm(options);
    return running.finished;
  }

  dataLossIfNamespaceDestroyed(namespace, destroyVolumes = false) {
    return withRpcError(this._inner.dataLossIfNamespaceDestroyed(namespace, destroyVolumes));
  }

  destroyNamespace(namespace, confirmDataLoss, destroyVolumes = false) {
    return withRpcError(this._inner.destroyNamespace(namespace, confirmDataLoss, destroyVolumes));
  }
}

forward(Client, native.Client, ["containerLogs", "logHistory", "metricHistory", "watch"]);

function wrapProgress(start, signal, wrap = value => value) {
  signal?.throwIfAborted();
  let pending;
  try { pending = start(); } catch (error) { throwRpcError(error); }
  const stop = () => pending.abort();
  signal?.addEventListener("abort", stop, { once: true });
  const finished = withRpcError(pending.finished()).then(wrap)
    .finally(() => signal?.removeEventListener("abort", stop));
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

function applyAll(namespace, specs, options = defaultPlanOptions()) {
  return {
    namespace,
    target: specs,
    options,
  };
}

function applyOne(namespace, spec, options = defaultPlanOptions()) {
  return {
    namespace,
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

async function openConfigStore(url, sealingSecret) {
  return bindAll(await withRpcError(native.openConfigStore(url, sealingSecret)), native.ConfigStore);
}

const observeVolumes = rpc(native.observeVolumes);
const observeCopies = rpc(native.observeCopies);
const allocateEnrollment = rpc(native.allocateEnrollment);
const machineUpgradeAttemptId = rpc(native.machineUpgradeAttemptId);
const ployzVersion = rpc(native.ployzVersion);
const agentTools = rpc(native.agentTools);
const agentStart = rpc(native.agentStart);
const agentResume = rpc(native.agentResume);
const agentCloudCall = rpc(native.agentCloudCall);

function setVolumeCopyRefusal(error) {
  const details = error?.details;
  if (details == null) return null;
  try {
    return native.setVolumeCopyRefusal(details);
  } catch {
    return null;
  }
}

// Shorthand entries only: Node's CommonJS export lexer stops at the first non-identifier value, and every name it
// misses is unavailable to `import { name } from "@ployz/sdk"`.
module.exports = {
  openConfigStore,
  observeVolumes,
  observeCopies,
  allocateEnrollment,
  setVolumeCopyRefusal,
  machineUpgradeAttemptId,
  ployzVersion,
  agentTools,
  agentStart,
  agentResume,
  agentCloudCall,
  connect,
  Client,
  RpcError,
  applyAll,
  applyOne,
};
