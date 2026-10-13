import { describe, expect, it } from "vitest";
import { runWords } from "./volume-copies";
import { run } from "./volume-run.test-fixture";

describe("runWords", () => {
  it("says each run in placement words", () => {
    expect(runWords(run("move", { to: "web-2" }, "running"))).toBe("Moving to web-2");
    expect(runWords(run("mirror", { to: "web-3" }, "running"))).toBe("Copying to web-3");
    expect(runWords(run("sync", { full: true }, "running"))).toBe("Refreshing mirror");
    expect(runWords(run("delete_mirror", { slot: "web-2", confirmed_name: null }, "running"))).toBe("Deleting the copy on web-2");
    expect(runWords(run("restore", { from: "web-2" }, "requested"))).toBe("Using mirror on web-2");
  });
});
