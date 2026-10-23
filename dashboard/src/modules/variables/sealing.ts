import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { SecretValue } from "@ployz/sdk";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { SealError, sealValue } from "./seal.client";

/** A Sealed value to send: its plaintext, and the variable it goes to. */
export type SealItem = {
  readonly key: string;
  readonly value: string;
  /** The receiving Environment's id and its Service's own id there. */
  readonly target: { readonly environment: string; readonly service: string };
};

const UNREAD = "Couldn't save this sealed value, so nothing was saved. Try again.";
const NO_SERVER = "Add a server to set sealed values.";

/**
 * Whether the Organization's Sealed controls show as sealing (`state`, cached, for display only), and `prepare`, which
 * turns Sealed values into what the Store takes: one fresh read per call, then the legacy plaintext while sealing is
 * off, or each value sealed to the key that read returned. It rejects with `SealError` and never falls back to plaintext.
 */
export function useSealing(organizationSlug: string) {
  const queryClient = useQueryClient();
  // Support can't read it; its controls show as off, and a Sealed write it tries rejects in `prepare`.
  const { data } = useQuery(sealingQueryOptions(organizationSlug));
  return {
    state: data?.sealing.state ?? "off",
    prepare: (items: readonly SealItem[]) => prepare(queryClient, organizationSlug, items),
  };
}

export type Sealing = ReturnType<typeof useSealing>;

async function prepare(queryClient: QueryClient, organizationSlug: string, items: readonly SealItem[]): Promise<SecretValue[]> {
  const fresh = await queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 })
    .catch(() => { throw new SealError(UNREAD); });
  const { sealing, organizationId } = fresh;
  if (sealing.state === "off") return items.map((item) => item.value);
  if (sealing.state === "no_server") throw new SealError(NO_SERVER);
  const recipient = { organizationId, publicKey: sealing.publicKey };
  return await Promise.all(items.map(async ({ key, value, target }): Promise<SecretValue> =>
    ({ sealedFor: target.environment, value: { sealed: await sealed(recipient, target, key, value), scope: { service: target.service }, key } })));
}

async function sealed(...input: Parameters<typeof sealValue>) {
  try {
    return await sealValue(...input);
  } catch (error) {
    throw error instanceof SealError ? error : new SealError(UNREAD);
  }
}
