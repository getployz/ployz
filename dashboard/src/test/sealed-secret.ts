import type { ClientSealedValue } from "@ployz/sdk";

/** Variable `key` as a client seals it for `target.service` in `target.environment`. The Store never checks the ciphertext itself. */
export const sealedSecret = (target: { readonly environment: string; readonly service: string }, key: string) => ({
  secret: {
    sealedFor: target.environment, value: { sealed: Buffer.alloc(48, 1).toString("base64"), scope: { service: target.service }, key },
  } satisfies ClientSealedValue,
});
