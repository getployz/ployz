"use strict";

const native = require("./ployz-sdk.node");

class Client {
  constructor(inner) {
    this._inner = inner;
    this.runtime = {
      watch: (options = {}) => iterateWatch(() => inner.watch(), options && options.signal),
    };
  }

  about() {
    return this._inner.about();
  }

  async preview(intent) {
    return wrapPreview(await this._inner.preview(intent));
  }

  async previewProjectRemoval(projectName, destroyVolumes) {
    return wrapPreview(
      await this._inner.previewProjectRemoval(projectName, destroyVolumes),
    );
  }

  async run(intent, options = {}) {
    const preview = await this.preview(intent);
    const running = preview.confirm(options);
    return running.finished;
  }

  removeVolumes(request) {
    return this._inner.removeVolumes(request);
  }

  dataLossIfMachineRemoved(machine) {
    return this._inner.dataLossIfMachineRemoved(machine);
  }

  removeMachine(machine, confirmDataLoss) {
    return this._inner.removeMachine(machine, confirmDataLoss);
  }

  dataLossIfProjectDestroyed(projectName, destroyVolumes = false) {
    return this._inner.dataLossIfProjectDestroyed(projectName, destroyVolumes);
  }

  destroyProject(projectName, confirmDataLoss, destroyVolumes = false) {
    return this._inner.destroyProject(projectName, confirmDataLoss, destroyVolumes);
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
      return wrapRunning(handle.confirm(), options && options.signal);
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
    stream = await start();
    if (signal?.aborted) {
      stream.cancel();
      return;
    }
    for (;;) {
      const value = await stream.next();
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
  return new Client(await native.connect(options));
}

module.exports = {
  connect,
  packageName: native.packageName,
  Client,
  applyAll,
  applyOne,
};
