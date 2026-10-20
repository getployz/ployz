import { STORE_UNMOUNT_DELAY } from "nanostores";
import { afterAll } from "vitest";

/** Vitest removes `window` when a jsdom file ends, so a store unmount still pending then throws in whatever runs next. */
export function failOnStoreUnmountAfterWindow() {
  afterAll(async () => {
    const jsdomWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
    Reflect.deleteProperty(globalThis, "window");
    await new Promise((resolve) => setTimeout(resolve, STORE_UNMOUNT_DELAY + 100));
    if (jsdomWindow) Object.defineProperty(globalThis, "window", jsdomWindow);
  });
}
