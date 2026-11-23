import type { Change, ClientSealedValue } from "@ployz/sdk";
import { expect, it } from "vitest";
import { admitDeploy } from "#/modules/config-store/admit-deploy";
import type { SealItem, Sealer } from "#/modules/sealing/use-sealing";
import type { DeployPlan } from "./deploy-tap.server";
import { fillPending } from "./fill-pending";

const GENERATED = '${{ secret(32, "a-zA-Z") }}';
const refusal: DeployPlan = {
  kind: "refused", message: "postgres.POSTGRES_PASSWORD has no value yet. Deploy from the dashboard to make it.",
  details: { environment: "env-1", pending: [{ service: "postgres", id: "svc-1", key: "POSTGRES_PASSWORD", generated: GENERATED }] },
};

/** A sealer that records each plaintext it seals, and an edit that answers `written`. */
function fill(version: string | null, written: number) {
  const plaintexts: SealItem[] = [];
  const sealer = Object.assign(async (item: SealItem): Promise<ClientSealedValue> => {
    plaintexts.push(item);
    return { sealedFor: item.target.environment, value: { sealed: "CIPHERTEXT", scope: { service: item.target.service }, key: item.key } };
  }, { publicKey: "key" }) satisfies Sealer;
  const sent: Change[][] = [];
  const command = admitDeploy({ project: "shop", environment: "pr-7" }, "deploy-1", { version });
  const filled = fillPending(command, refusal, async () => sealer, async (changes) => {
    sent.push(changes);
    return written;
  });
  return { filled, plaintexts, sent };
}

it("makes and seals each pending value in the browser, and Cloud gets only its ciphertext", async () => {
  const { filled, plaintexts, sent } = fill("7:3:2.1", 8);
  await filled;
  const [made] = plaintexts;
  expect(made?.value).toMatch(/^[a-zA-Z]{32}$/u);
  expect(made?.target).toEqual({ environment: "env-1", service: "svc-1" });
  expect(sent).toEqual([[{ op: "set", path: "postgres.env.POSTGRES_PASSWORD",
    value: { secret: { sealedFor: "env-1", value: { sealed: "CIPHERTEXT", scope: { service: "svc-1" }, key: "POSTGRES_PASSWORD" }, generated: GENERATED } } }]]);
  expect(JSON.stringify(sent)).not.toContain(made?.value);
});

it("moves the reviewed version past its own write, its loss binding kept", async () => {
  expect((await fill("7:3:2.1#loss-abc", 8).filled)?.version).toBe("8:3:2.1#loss-abc");
});

it("leaves the version when another change landed before the fill, so the Deploy goes back to review", async () => {
  expect((await fill("7:3:2.1", 9).filled)?.version).toBe("7:3:2.1");
});

it("leaves a Deploy alone when nothing is pending", async () => {
  const command = admitDeploy({ project: "shop", environment: "pr-7" }, "deploy-1");
  const refused: DeployPlan = { kind: "refused", message: "No.", details: null };
  expect(await fillPending(command, refused, () => Promise.reject(new Error("sealed")), () => Promise.reject(new Error("wrote")))).toBeNull();
});
