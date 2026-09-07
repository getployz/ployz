import type { VariableRecord } from "#/modules/environment-design/variables";
import { extractDisplayRefs } from "#/modules/environment-design/variable-template";

export type ReferenceConsumer = {
  variableId: string;
  variableKey: string;
  /** The producer KEY this consumer references. */
  key: string;
};

type ConsumerVariable = Pick<VariableRecord, "id" | "key" | "value">;

/**
 * Variables whose plain value references any key of the producer with `ownerSlug`.
 * Drives the non-blocking "deleting X affects these" impact warning. Matching is
 * by current slug, which is correct for a delete/rename happening now.
 */
export function findConsumersOfOwner(
  variables: ConsumerVariable[],
  ownerSlug: string,
): ReferenceConsumer[] {
  const consumers: ReferenceConsumer[] = [];
  for (const variable of variables) {
    if (variable.value.type !== "plain") continue;
    for (const ref of extractDisplayRefs(variable.value.value)) {
      if (ref.ownerSlug === ownerSlug) {
        consumers.push({
          variableId: variable.id,
          variableKey: variable.key,
          key: ref.key,
        });
      }
    }
  }
  return consumers;
}

/** Consumers that reference a specific `ownerSlug.key` (for KEY-rename impact). */
export function findConsumersOfKey(
  variables: ConsumerVariable[],
  ownerSlug: string,
  key: string,
): ReferenceConsumer[] {
  return findConsumersOfOwner(variables, ownerSlug).filter(
    (consumer) => consumer.key === key,
  );
}
