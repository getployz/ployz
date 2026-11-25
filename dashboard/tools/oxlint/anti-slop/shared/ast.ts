import type { ESTree, Scope, SourceCode, Variable } from "@oxlint/plugins";

type VisitorKeys = Readonly<Record<string, readonly string[]>>;

function isNode(value: unknown): value is ESTree.Node {
  return typeof value === "object" && value !== null && "type" in value && typeof value.type === "string";
}

/** A property key's name, when it is written out. */
export function propertyName(node: ESTree.Node): string | null {
  if (node.type === "Identifier") return node.name;
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  return null;
}

/** The variable an identifier names, from the innermost scope outward; null for an undeclared global. */
export function resolveVariable(sourceCode: SourceCode, identifier: ESTree.Node & { name: string }): Variable | null {
  let scope: Scope | null = sourceCode.getScope(identifier);
  while (scope !== null) {
    const variable = scope.set.get(identifier.name);
    if (variable !== undefined) return variable;
    scope = scope.upper;
  }
  return null;
}

/** A node's direct children, in source order. */
export function childNodes(node: ESTree.Node, visitorKeys: VisitorKeys): Array<ESTree.Node> {
  const record = node as unknown as Readonly<Record<string, unknown>>;
  const children: Array<ESTree.Node> = [];
  for (const key of visitorKeys[node.type] ?? []) {
    const value = record[key];
    if (isNode(value)) children.push(value);
    else if (Array.isArray(value)) for (const child of value) if (isNode(child)) children.push(child);
  }
  return children;
}

/** Identifiers read in an expression, skipping property names and the bodies of nested functions. */
export function readIdentifiers(node: ESTree.Node, visitorKeys: VisitorKeys): Array<ESTree.IdentifierReference> {
  if (node.type === "Identifier") return [node as ESTree.IdentifierReference];
  if (node.type === "ArrowFunctionExpression" || node.type === "FunctionExpression") return [];
  return childNodes(node, visitorKeys).flatMap((child) => {
    if (node.type === "MemberExpression" && child === node.property && !node.computed) return [];
    if (node.type === "Property" && child === node.key && !node.computed) return [];
    return readIdentifiers(child, visitorKeys);
  });
}

export function within(outer: ESTree.Node, inner: ESTree.Node): boolean {
  return outer.start <= inner.start && inner.end <= outer.end;
}
