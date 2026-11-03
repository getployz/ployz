import { inspect } from "node:util";
import { vi } from "vitest";

/**
 * Counts the lines logged at `level` that hold every one of `texts`, until disposed. Effect's default logger writes
 * every level through `console.log`, naming the level before the message.
 */
export const logged = (level: "WARN" | "ERROR", ...texts: readonly string[]) => {
  const spy = vi.spyOn(console, "log");
  return {
    count: () => spy.mock.calls.map((args) => inspect(args, { depth: null }))
      .filter((line) => line.includes(` ${level} `) && texts.every((text) => line.includes(text))).length,
    [Symbol.dispose]: () => spy.mockRestore(),
  };
};
