import { STORE_UNMOUNT_DELAY } from "nanostores";
import { afterAll, afterEach } from "vitest";

/** Vitest removes `window` when a jsdom file ends, so a store unmount still pending then throws in whatever runs next. */
export function failOnStoreUnmountAfterWindow() {
  afterAll(async () => {
    await withoutWindow(STORE_UNMOUNT_DELAY + 100);
  });
}

/**
 * A render still in flight when a test ends flushes on a later task, after cleanup and possibly after `window` is gone.
 * Register it before the file's cleanup hook: Vitest runs `afterEach` hooks in reverse order, so it runs after unmount.
 */
export function failOnRenderAfterTest() {
  afterEach(async () => {
    await withoutWindow(50);
  });
}

async function withoutWindow(ms: number) {
  const jsdomWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Reflect.deleteProperty(globalThis, "window");
  await new Promise((resolve) => setTimeout(resolve, ms));
  if (jsdomWindow) Object.defineProperty(globalThis, "window", jsdomWindow);
}
