import { expect, it } from "vitest";
import { formatDuration } from "./relative-time";

it("words a duration in whole seconds, minutes past a minute, hours past an hour", () => {
  expect([0, 45.9, 72, 3599, 7500].map(formatDuration)).toEqual(["0s", "45s", "1m 12s", "59m 59s", "2h 5m"]);
});
