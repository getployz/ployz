import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import type { ClientSealedValue, SecretValue } from "@ployz/sdk";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { SealError } from "#/modules/sealing/seal-error";
import { isUnauthorized } from "#/lib/error-message";
import { sealValue } from "./seal-value";

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
 * Whether the Organization's Sealed controls show as ready (`state`, cached, for display only; undefined while it
 * loads), and `prepare`, which turns Sealed values into what the Store takes: one fresh read per call, then each value
 * sealed to the key that read returned. It rejects with `SealError` and never falls back to plaintext. `sealer` is that
 * read alone, for a write that must find where its values go.
 */
export function useSealing(organizationSlug: string) {
  const queryClient = useQueryClient();
  const { data } = useQuery(sealingQueryOptions(organizationSlug));
  return {
    state: data?.sealing.state,
    prepare: async (items: readonly SealItem[]): Promise<SecretValue[]> => {
      const seal = await sealer(queryClient, organizationSlug);
      return Promise.all(items.map(seal));
    },
    sealer: () => sealer(queryClient, organizationSlug),
  };
}

export type Sealing = ReturnType<typeof useSealing>;

/** Seals one item to the key a fresh read returned, `publicKey`. */
export type Sealer = ((item: SealItem) => Promise<ClientSealedValue>) & { readonly publicKey: string };

/** One fresh read: a `Sealer` bound to the key it returned. */
async function sealer(queryClient: QueryClient, organizationSlug: string): Promise<Sealer> {
  const fresh = await queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 })
    .catch((error) => { throw isUnauthorized(error) ? error : new SealError(UNREAD); });
  const { sealing, organizationId } = fresh;
  if (sealing.state === "no_server") throw new SealError(NO_SERVER);
  const recipient = { organizationId, publicKey: sealing.publicKey };
  const seal = async ({ key, value, target }: SealItem): Promise<ClientSealedValue> =>
    ({ sealedFor: target.environment, value: { sealed: await sealed(recipient, target, key, value), scope: { service: target.service }, key } });
  return Object.assign(seal, { publicKey: sealing.publicKey });
}

async function sealed(...input: Parameters<typeof sealValue>) {
  try {
    return await sealValue(...input);
  } catch (error) {
    throw error instanceof SealError ? error : new SealError(UNREAD);
  }
}
