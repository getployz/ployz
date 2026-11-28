import { describe, expect, it } from "vitest";
import { removedToast } from "./removed-toast";

describe("removedToast", () => {
  it("says Cloud keeps its hold when it does", () => {
    const reason = "Cloud was paired with a new Cluster since.";
    expect(removedToast("web-1", { release: { kind: "kept", reason } })).toEqual({
      warning: true,
      message: `web-1 left the cluster. Cloud keeps its hold: ${reason}`,
    });
  });

  it("says to add a server once the last one is removed", () => {
    expect(removedToast("web-1", { release: { kind: "released" } }))
      .toEqual({ warning: false, message: "web-1 removed. Add a server to deploy again." });
  });

  it("says a removal with other Servers left plainly", () => {
    expect(removedToast("web-2", { release: { kind: "others_remain" } }))
      .toEqual({ warning: false, message: "web-2 removed" });
  });
});
