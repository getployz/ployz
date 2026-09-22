import { expect, it } from "vitest";
import { preparationProgressCollector } from "./preparation-progress";

it("keeps output beyond the former byte and chunk caps, including the final error", () => {
  const progress = preparationProgressCollector();
  let stored = "";
  const chunk = Array.from(Buffer.alloc(8192, 65));
  for (let i = 0; i < 1000; i++) {
    stored += progress.event({ Build: { Output: chunk } })?.output ?? "";
  }
  stored += progress.event({ Build: { Output: Array.from(Buffer.from("Final build error")) } })?.output ?? "";
  expect(stored).toBe("A".repeat(8192 * 1000) + "Final build error");
  expect(progress.current().outputTruncated).toBe(false);
  expect(progress.current().output).toBe("");
  expect(progress.event("Transfer")?.phase).toBe("transfer");
});

it("preserves UTF-8 split between output events", () => {
  const progress = preparationProgressCollector();
  const bytes = Buffer.from("error: 🐴\n");
  const first = progress.event({ Build: { Output: Array.from(bytes.subarray(0, 9)) } });
  const second = progress.event({ Build: { Output: Array.from(bytes.subarray(9)) } });
  expect((first?.output ?? "") + (second?.output ?? "")).toBe("error: 🐴\n");
});
