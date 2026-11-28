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

  it("says the last Server still runs its containers and how to stop them", () => {
    expect(removedToast("web-1", { release: { kind: "released" } }).message)
      .toMatch(/^web-1 removed\. It still runs its containers: run sudo ployz-uninstall on it .* ployz server add --reset/);
  });

  it("says a removal with other Servers left plainly", () => {
    expect(removedToast("web-2", { release: { kind: "others_remain" } }))
      .toEqual({ warning: false, message: "web-2 removed" });
  });
});
