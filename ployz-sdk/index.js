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

  preview(intent) {
    return this._inner.preview(intent);
  }

  deploy(intent) {
    return this._inner.deploy(intent);
  }

  removeVolumes(request) {
    return this._inner.removeVolumes(request);
  }

  close() {
    return this._inner.close();
  }
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

async function connect(options) {
  return new Client(await native.connect(options));
}

module.exports = {
  connect,
  packageName: native.packageName,
  Client,
};
