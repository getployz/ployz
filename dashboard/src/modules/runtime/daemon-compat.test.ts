import { describe, expect, it } from "vitest";
import { lacks, requirementFor, requirements, ServerLacks, serverSupports } from "./daemon-compat";
import { asSdkFailure, PloyzProviderError, sdkFailureMessage } from "./ployz.server";

describe("lacks", () => {
  it("splits the Servers that named a capability from every other failure", () => {
    const ordinary = { machineId: "fsn-1", machineName: "fsn-1", message: "Its log service isn’t running." };
    const split = lacks([
      { machineId: "hel-1", machineName: "hel-1", message: "unsupported", code: "unsupported", capability: "ployz.logs.history.v2" },
      ordinary,
      { machineId: "nbg-1", machineName: "nbg-1", message: "refused", code: "unsupported" },
    ]);
    expect(split.lacks).toEqual([new ServerLacks({ machineId: "hel-1", capability: "ployz.logs.history.v2" })]);
    expect(split.rest.map(failure => failure.machineId)).toEqual(["fsn-1", "nbg-1"]);
  });
});

describe("serverSupports", () => {
  it("reads the Server's record, and claims nothing when the record lists no capabilities", () => {
    expect(serverSupports({ capabilities: [requirements.liveLogs.capability] }, requirements.liveLogs)).toBe(true);
    expect(serverSupports({ capabilities: [requirements.liveLogs.capability] }, requirements.volumeMove)).toBe(false);
    expect(serverSupports({ capabilities: [] }, requirements.volumeMove)).toBe("unknown");
  });

  it("names the feature a refused capability belongs to", () => {
    expect(requirementFor("ployz.logs.tail.v1")).toBe(requirements.liveLogs);
    expect(requirementFor("ployz.something.v9")).toBeNull();
  });
});

/** The error @ployz/sdk throws for a daemon's refusal, as its index.js builds it from the native `PLOYZ_RPC_ERROR:` message. */
function rpcError(payload: { code: string; message: string; details: Record<string, string> }) {
  const native = new Error(`PLOYZ_RPC_ERROR:${JSON.stringify(payload)}`);
  return Object.assign(new Error(payload.message, { cause: native }), { name: "RpcError", code: payload.code, details: payload.details });
}

describe("asSdkFailure", () => {
  it("turns a refusal that names a capability into ServerLacks for the Server it addressed", () => {
    const failure = asSdkFailure("logs", rpcError({ code: "unsupported", message: "", details: { capability: "ployz.logs.tail.v1", target: "machine-1" } }));
    expect(failure).toEqual(new ServerLacks({ machineId: "machine-1", capability: "ployz.logs.tail.v1" }));
  });

  it("keeps an unsupported refusal without a capability as the provider's own failure", () => {
    const failure = asSdkFailure("logs", rpcError({ code: "unsupported", message: "disabled", details: { cause: "disabled" } }));
    expect(failure).toBeInstanceOf(PloyzProviderError);
    expect(sdkFailureMessage(failure)).toBe("unsupported: disabled");
  });
});

describe("sdkFailureMessage", () => {
  it("names the lacking Server and agrees with a plural feature", () => {
    const servers = [{ id: "machine-1", name: "fsn-1" }, { id: "machine-2", name: "hel-1" }];
    expect(sdkFailureMessage(new ServerLacks({ machineId: "machine-2", capability: requirements.liveLogs.capability }), servers)).toBe("Logs need a newer Ployz on hel-1.");
    expect(sdkFailureMessage(new ServerLacks({ machineId: "machine-1", capability: requirements.volumeMove.capability }), servers)).toBe("Volume move needs a newer Ployz on fsn-1.");
    expect(sdkFailureMessage(new ServerLacks({ machineId: "machine-9", capability: "ployz.something.v9" }))).toBe("This step needs a newer Ployz on one of your Servers.");
  });
});
