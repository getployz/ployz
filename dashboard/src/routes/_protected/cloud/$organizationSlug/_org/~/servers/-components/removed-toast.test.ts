import { describe, expect, it } from "vitest";
import { removedToast } from "./removed-toast";

const unconfirmed = "its reset couldn't be confirmed: it was already gone from the Cluster";

describe("removedToast", () => {
  it("says Cloud let go of a Server already gone from the Cluster, and that its reset is unconfirmed", () => {
    expect(removedToast("web-2", { resetWarning: unconfirmed, release: { kind: "others_remain" } })).toEqual({
      warning: true,
      message: "web-2 removed, but its reset couldn't be confirmed: it was already gone from the Cluster.",
    });
  });

  it("keeps saying Cloud keeps its hold on the last Server", () => {
    const reason = `its reset didn't finish (${unconfirmed}), so it may still hold Cloud's key.`;
    expect(removedToast("web-1", { resetWarning: unconfirmed, release: { kind: "kept", reason } })).toEqual({
      warning: true,
      message: `web-1 left the cluster. Cloud keeps its hold: ${reason}`,
    });
  });

  it("says a clean removal plainly", () => {
    expect(removedToast("web-2", { resetWarning: null, release: { kind: "others_remain" } }))
      .toEqual({ warning: false, message: "web-2 removed" });
    expect(removedToast("web-1", { resetWarning: null, release: { kind: "released" } }))
      .toEqual({ warning: false, message: "web-1 removed. Add a server to deploy again." });
  });
});
