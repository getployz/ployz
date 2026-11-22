// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, renderHook } from "@testing-library/react";
import { CipherSuite } from "@hpke/core";
import { Chacha20Poly1305 } from "@hpke/chacha20poly1305";
import { DhkemX25519HkdfSha256, HkdfSha256 } from "@hpke/dhkem-x25519";
import type { ClientSealedValue } from "@ployz/sdk";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as functions from "#/modules/sealing/sealing.functions";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { SealError } from "#/modules/sealing/seal-error";
import { useSealing } from "./sealing";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const suite = new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Chacha20Poly1305() });
const target = { environment: "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f", service: "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6" };
const item = { key: "API_TOKEN", value: "tok_live_123456", target };

async function keyPair() {
  const pair = await suite.kem.generateKeyPair();
  const raw = new Uint8Array(await suite.kem.serializePublicKey(pair.publicKey));
  return { privateKey: pair.privateKey, publicKey: btoa(String.fromCharCode(...raw)) };
}

/** Whether `sealed` opens with `privateKey`; the AAD is checked by `seal-value.test.ts`, so any AAD failure is a wrong key here too. */
async function opensWith(sealed: ClientSealedValue, privateKey: CryptoKeyPair["privateKey"]) {
  const bytes = Uint8Array.from(atob(sealed.value.sealed), (char) => char.charCodeAt(0));
  const recipient = await suite.createRecipientContext({ recipientKey: privateKey, enc: bytes.slice(0, 32).buffer, info: new TextEncoder().encode("ployz-sealed-v1").buffer });
  return await recipient.open(bytes.slice(32).buffer, associatedData(sealed)).then(() => true, () => false);
}

function associatedData(sealed: ClientSealedValue) {
  const uuid = (text: string) => Uint8Array.from(text.replaceAll("-", "").match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
  const scope = sealed.value.scope;
  const fields = [new TextEncoder().encode("ployz-sealed-v1"), new TextEncoder().encode("org_fresh"), uuid(sealed.sealedFor),
    new TextEncoder().encode("service"), uuid(scope === "shared" ? "" : scope.service), new TextEncoder().encode(sealed.value.key)];
  const out = new Uint8Array(fields.reduce((length, field) => length + 4 + field.length, 0));
  let offset = 0;
  for (const field of fields) {
    new DataView(out.buffer).setUint32(offset, field.length);
    out.set(field, offset + 4);
    offset += 4 + field.length;
  }
  return out.buffer;
}

type Read = Awaited<ReturnType<typeof functions.readSealingFn>>;

/** A tab that has `cached` (or nothing yet), over a Cloud that now answers `fresh`. */
function setup(cached: Read | undefined, fresh: () => Promise<Read>) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  if (cached) queryClient.setQueryData(sealingQueryOptions("acme").queryKey, cached);
  const read = vi.spyOn(functions, "readSealingFn").mockImplementation(fresh);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  return { read, sealing: renderHook(() => useSealing("acme"), { wrapper }).result.current };
}

it("seals with the key a fresh read returns, never the cached one", async () => {
  const [a, b] = await Promise.all([keyPair(), keyPair()]);
  const test = setup({ organizationId: "org_cached", sealing: { state: "ready", publicKey: a.publicKey } }, async () => ({ organizationId: "org_fresh", sealing: { state: "ready", publicKey: b.publicKey } }));

  expect(test.sealing.state).toBe("ready");
  const [sealed] = await test.sealing.prepare([item]);

  expect(test.read).toHaveBeenCalledTimes(1);
  // SAFETY: a ready read seals every item.
  const value = sealed as ClientSealedValue;
  expect(value).toMatchObject({ sealedFor: target.environment, value: { scope: { service: target.service }, key: "API_TOKEN" } });
  expect(JSON.stringify(value)).not.toContain(item.value);
  expect(await opensWith(value, b.privateKey)).toBe(true);
  expect(await opensWith(value, a.privateKey)).toBe(false);
});

it("reads afresh before any read lands, and shows no state until one does", async () => {
  const { publicKey } = await keyPair();
  const test = setup(undefined, async () => ({ organizationId: "org_fresh", sealing: { state: "ready", publicKey } }));

  expect(test.sealing.state).toBeUndefined();
  expect(await test.sealing.prepare([item])).toMatchObject([{ sealedFor: target.environment }]);
});

it("rejects without the value when there is no server or the read fails", async () => {
  const { publicKey } = await keyPair();
  const noServer = setup({ organizationId: "org", sealing: { state: "ready", publicKey } }, async () => ({ organizationId: "org", sealing: { state: "no_server" } }));
  await expect(noServer.sealing.prepare([item])).rejects.toEqual(new SealError("Add a server to set sealed values."));

  cleanup();
  vi.restoreAllMocks();
  const failed = setup({ organizationId: "org", sealing: { state: "no_server" } }, async () => { throw new Error(`boom ${item.value}`); });
  await expect(failed.sealing.prepare([item])).rejects.toEqual(new SealError("Couldn't save this sealed value, so nothing was saved. Try again."));
});

it("refuses a value the browser can't seal", async () => {
  const { publicKey } = await keyPair();
  const test = setup(undefined, async () => ({ organizationId: "org", sealing: { state: "ready", publicKey } }));

  await expect(test.sealing.prepare([{ ...item, key: "PORT", value: "8080" }])).rejects.toEqual(new SealError("PORT can't be sealed: Ployz reads it to route traffic."));
});
