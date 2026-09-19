import type { EnvironmentSnapshotVariableProducer, ValuePart } from "#/modules/environment-design/tables";

export type ResolvedVariableProducer = Omit<EnvironmentSnapshotVariableProducer, "value"> & {
  value: { kind: "literal" | "secret"; value: string } | { kind: "template"; parts: ValuePart[] };
};

/** Resolve decrypted producer values; cycles fail and missing references resolve to "". */
export function resolveVariableParts(
  parts: ValuePart[],
  selfOwnerId: string,
  inputProducers: ResolvedVariableProducer[],
): string {
  // Latest producer wins, including a Service variable overriding a managed default.
  const producers = [...inputProducers].reverse();
  const memo = new Map<string, string>();
  const resolveParts = (parts: ValuePart[], selfOwnerId: string, stack: string[]): string => {
    return parts.map((part) => {
      if (part.kind === "text") return part.value;
      const owner = part.owner;
      // ponytail: linear lookup; index by owner/key if large environments make it measurable.
      const producer = producers.find((candidate) => candidate.key === part.key &&
        (owner.scope === "self" ? candidate.ownerId === selfOwnerId
          : candidate.ownerScope === owner.scope && candidate.ownerLineageId === owner.lineageId));
      if (!producer) return "";
      const identity = `${producer.ownerId}::${producer.key}`;
      if (stack.includes(identity)) throw new Error(`Circular variable reference: ${[...stack, identity].join(" -> ")}`);
      const cached = memo.get(identity);
      if (cached !== undefined) return cached;
      const value = producer.value.kind === "template"
        ? resolveParts(producer.value.parts, producer.ownerId, [...stack, identity])
        : producer.value.value;
      memo.set(identity, value);
      return value;
    }).join("");
  };

  return resolveParts(parts, selfOwnerId, []);
}
