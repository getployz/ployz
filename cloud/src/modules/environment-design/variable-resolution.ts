import { Data, Result } from "effect";
import type { ValuePart, ValuePartRefOwner } from "#/modules/environment-design/tables";

/** A templated value contains a circular chain of variable references. */
export class TemplateResolveError extends Data.TaggedError(
  "TemplateResolveError",
)<{
  message: string;
  code: "cycle";
  path: string[];
}> {}

/**
 * Pure, isomorphic resolver for templated variable values. It walks a value's
 * `ValuePart[]`, substituting `ref` parts with their producers' resolved values,
 * recursing through producers that are themselves templated, while detecting
 * cycles and propagating "secret-ness".
 *
 * It is deliberately decoupled from the data layer: callers supply a `lookup`
 * that maps a ref to a concrete producer in the target environment (resolving
 * `lineageId` -> the env's service/group, and `self` -> the consuming owner).
 * The deploy path builds that producer map; tests build it inline.
 */

export type ResolverProducer =
  | { kind: "literal"; value: string }
  | { kind: "secret"; value: string }
  | { kind: "template"; parts: ValuePart[] };

/**
 * Resolve a ref against the target environment. `selfOwnerId` is the concrete
 * per-environment id of the owner that holds the value currently being
 * resolved (used for `{ scope: "self" }` refs). Returns the producer plus the
 * concrete `ownerId` that holds it (needed as the cycle/memo identity and as
 * the `selfOwnerId` when recursing into a templated producer), or null when the
 * referenced producer/key does not exist in this environment.
 */
export type ProducerLookup = (input: {
  owner: ValuePartRefOwner;
  selfOwnerId: string;
  key: string;
}) => { ownerId: string; producer: ResolverProducer } | null;

export type TemplateWarning = { kind: "missing"; ownerId: string | null; key: string };

export type ResolveOutcome = {
  value: string;
  /** True when any (transitive) part came from a secret producer. */
  secret: boolean;
  warnings: TemplateWarning[];
};

function nodeId(ownerId: string, key: string): string {
  return `${ownerId}::${key}`;
}

/**
 * Resolve a templated value to its final string. Cycles produce a
 * `TemplateResolveError`; missing references resolve to `""` and are reported in
 * `warnings` (deploy proceeds, per product decision).
 */
export function resolveValueParts(input: {
  parts: ValuePart[];
  selfOwnerId: string;
  lookup: ProducerLookup;
}): Result.Result<ResolveOutcome, TemplateResolveError> {
  const warnings: TemplateWarning[] = [];
  const memo = new Map<string, { value: string; secret: boolean }>();
  const visiting = new Set<string>();
  const stack: string[] = [];

  function resolveParts(
    parts: ValuePart[],
    selfOwnerId: string,
  ): Result.Result<{ value: string; secret: boolean }, TemplateResolveError> {
    let value = "";
    let secret = false;

    for (const part of parts) {
      if (part.kind === "text") {
        value += part.value;
        continue;
      }

      const found = input.lookup({
        owner: part.owner,
        selfOwnerId,
        key: part.key,
      });
      if (!found) {
        warnings.push({
          kind: "missing",
          ownerId: part.owner.scope === "self" ? selfOwnerId : null,
          key: part.key,
        });
        continue;
      }

      const id = nodeId(found.ownerId, part.key);
      const cached = memo.get(id);
      if (cached) {
        value += cached.value;
        secret ||= cached.secret;
        continue;
      }

      if (visiting.has(id)) {
        return Result.fail(
          new TemplateResolveError({
            message: `Circular variable reference: ${[...stack, id].join(" -> ")}`,
            code: "cycle",
            path: [...stack, id],
          }),
        );
      }

      let resolved;
      if (found.producer.kind === "literal") {
        resolved = { value: found.producer.value, secret: false };
      } else if (found.producer.kind === "secret") {
        resolved = { value: found.producer.value, secret: true };
      } else {
        visiting.add(id);
        stack.push(id);
        const inner = resolveParts(found.producer.parts, found.ownerId);
        stack.pop();
        visiting.delete(id);
        if (Result.isFailure(inner)) return inner;
        resolved = inner.success;
      }

      memo.set(id, resolved);
      value += resolved.value;
      secret ||= resolved.secret;
    }

    return Result.succeed({ value, secret });
  }

  const result = resolveParts(input.parts, input.selfOwnerId);
  if (Result.isFailure(result)) return Result.fail(result.failure);
  return Result.succeed({ ...result.success, warnings });
}
