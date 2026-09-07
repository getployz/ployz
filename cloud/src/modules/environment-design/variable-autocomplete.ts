import type { CaretToken } from "#/modules/environment-design/variable-template";

/**
 * A producer a value can reference via `${{ }}`. Pure data shared by the
 * autocomplete hook and its tests; the React layer derives these from the live
 * services/variable-group collections.
 */
export type ReferenceTargetKind = "self" | "service" | "variable_group" | "managed";

export type ReferenceTarget = {
  key: string;
  /** Slug to prefix in the inserted token, or null for a self reference. */
  ownerSlug: string | null;
  kind: ReferenceTargetKind;
  /** Owner label shown as the suggestion's source (service/group name, or "Managed"). */
  ownerLabel: string;
  isSecret: boolean;
  description: string | null;
};

type OwnerVariable = {
  key: string;
  exported: boolean;
  isSecret: boolean;
  description: string | null;
};

type ServiceProducer = {
  slug: string;
  name: string;
  isSelf: boolean;
  variables: OwnerVariable[];
  managedExports: { key: string; description: string | null }[];
};

type VariableGroupProducer = {
  slug: string;
  name: string;
  isSelf: boolean;
  variables: OwnerVariable[];
};

/**
 * Build the reference targets offered while editing a value. Service-owned values
 * may reference their own variables + managed exports (no prefix), plus other
 * services' exported variables/managed exports and variable groups' exported
 * variables (prefixed by slug). Variable-group-owned values may only reference
 * their own variables, so cross-owner producers are dropped.
 */
export function buildReferenceTargets(input: {
  ownerScope: "service" | "variable_group";
  services: ServiceProducer[];
  variableGroups: VariableGroupProducer[];
}): ReferenceTarget[] {
  const targets: ReferenceTarget[] = [];

  const selfVariables = (
    input.ownerScope === "service"
      ? input.services.find((service) => service.isSelf)?.variables
      : input.variableGroups.find((variableGroup) => variableGroup.isSelf)
          ?.variables
  ) ?? [];
  for (const variable of selfVariables) {
    targets.push({
      key: variable.key,
      ownerSlug: null,
      kind: "self",
      ownerLabel:
        "This " + (input.ownerScope === "service" ? "service" : "group"),
      isSecret: variable.isSecret,
      description: variable.description,
    });
  }

  if (input.ownerScope === "variable_group") {
    return targets;
  }

  const selfService = input.services.find((service) => service.isSelf);
  for (const exported of selfService?.managedExports ?? []) {
    targets.push({
      key: exported.key,
      ownerSlug: null,
      kind: "managed",
      ownerLabel: "Managed",
      isSecret: false,
      description: exported.description,
    });
  }

  for (const service of input.services) {
    if (service.isSelf) continue;
    for (const variable of service.variables) {
      if (!variable.exported) continue;
      targets.push({
        key: variable.key,
        ownerSlug: service.slug,
        kind: "service",
        ownerLabel: service.name,
        isSecret: variable.isSecret,
        description: variable.description,
      });
    }
    for (const exported of service.managedExports) {
      targets.push({
        key: exported.key,
        ownerSlug: service.slug,
        kind: "managed",
        ownerLabel: service.name,
        isSecret: false,
        description: exported.description,
      });
    }
  }

  for (const variableGroup of input.variableGroups) {
    for (const variable of variableGroup.variables) {
      if (!variable.exported) continue;
      targets.push({
        key: variable.key,
        ownerSlug: variableGroup.slug,
        kind: "variable_group",
        ownerLabel: variableGroup.name,
        isSecret: variable.isSecret,
        description: variable.description,
      });
    }
  }

  return targets;
}

function startsWithCi(value: string, prefix: string): boolean {
  return value.toLowerCase().startsWith(prefix.toLowerCase());
}

/**
 * Filter and rank targets for the token under the caret. With a typed owner slug
 * (`${{ db.`) only that owner's keys match; otherwise both self keys and owner
 * slugs are matched against the partial query so a user can start with either.
 */
export function filterReferenceTargets(
  targets: ReferenceTarget[],
  token: CaretToken,
): ReferenceTarget[] {
  const matched = targets.filter((target) => {
    if (token.ownerSlug != null) {
      return (
        target.ownerSlug === token.ownerSlug && startsWithCi(target.key, token.query)
      );
    }
    if (token.query === "") return true;
    return (
      startsWithCi(target.key, token.query) ||
      (target.ownerSlug != null && startsWithCi(target.ownerSlug, token.query))
    );
  });

  const kindOrder = {
    self: 0,
    managed: 1,
    service: 2,
    variable_group: 3,
  } as const satisfies Record<ReferenceTargetKind, number>;
  return matched.sort((left, right) => {
    if (left.kind !== right.kind) return kindOrder[left.kind] - kindOrder[right.kind];
    const ownerDelta = (left.ownerSlug ?? "").localeCompare(right.ownerSlug ?? "");
    if (ownerDelta !== 0) return ownerDelta;
    return left.key.localeCompare(right.key);
  });
}
