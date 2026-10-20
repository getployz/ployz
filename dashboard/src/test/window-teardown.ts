import { afterAll, afterEach, expect } from "vitest";

/**
 * Vitest removes `window` when a jsdom file ends, so a store still mounted then unmounts a second later and throws in
 * whatever file runs next. Nano Stores keeps `active` set from mount until that delayed unmount runs.
 */
export function failOnMountedStore(name: string, store: () => { active?: boolean; lc: number } | undefined) {
  afterAll(() => {
    expect(store()?.active ?? false, `${name} store is still mounted when the file ends`).toBe(false);
  });
}

/**
 * A render still in flight when a test ends flushes on a later task, after cleanup and possibly after `window` is gone.
 * Register it before the file's cleanup hook: Vitest runs `afterEach` hooks in reverse order, so it runs after unmount.
 */
export function failOnRenderAfterTest() {
  afterEach(async () => {
    const reads = await reactWindowReads(50);
    expect(reads[0], "React work ran after the test's cleanup").toBeUndefined();
  });
}

async function reactWindowReads(ms: number) {
  const jsdomWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  if (!jsdomWindow) return [];
  const value: unknown = jsdomWindow.get ? jsdomWindow.get.call(globalThis) : jsdomWindow.value;
  const reads: string[] = [];
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    get() {
      const stack = new Error("window read").stack ?? "";
      if (stack.includes("/react-dom/")) reads.push(stack);
      return value;
    },
  });
  try {
    await new Promise((resolve) => setTimeout(resolve, ms));
  } finally {
    Object.defineProperty(globalThis, "window", jsdomWindow);
  }
  return reads;
}
