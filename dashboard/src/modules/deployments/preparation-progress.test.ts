import { expect, it } from "vitest";
import { BUILD_OUTPUT_LIMIT, preparationProgressCollector } from "./preparation-progress";

it("bounds stored build output and stops appending after truncation", () => {
  const progress = preparationProgressCollector();
  let stored = 0;
  for (let i = 0; i < 1000; i++) {
    const event = progress.event({ Build: { Output: Array.from(Buffer.alloc(1024, 65)) } });
    stored += Buffer.byteLength(event?.output ?? "");
  }
  expect(stored).toBe(BUILD_OUTPUT_LIMIT);
  expect(progress.current().outputTruncated).toBe(true);
  expect(progress.current().output).toBe("");
  expect(progress.event("Transfer")?.phase).toBe("transfer");
});
