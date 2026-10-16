import { defineRule, eslintCompatPlugin } from "@oxlint/plugins";
import type { ESTree } from "@oxlint/plugins";
import fs from "node:fs";
import path from "node:path";
import { parseSync } from "oxc-parser";

/** What a settings card used to be built from, before every row had an archetype. */
const LEGACY_ROWS = new Set(["Field", "FieldGroup", "FieldSet", "Item", "ItemGroup", "Separator", "DangerRow"]);

/** The JSX a section's card renders as its own children, looking through fragments, conditionals and `&&`. */
function cardChildren(nodes: readonly ESTree.Node[]): ESTree.JSXElement[] {
  return nodes.flatMap((node): ESTree.JSXElement[] => {
    switch (node.type) {
      case "JSXElement":
        return [node];
      case "JSXFragment":
        return cardChildren(node.children);
      case "JSXExpressionContainer":
        return node.expression.type === "JSXEmptyExpression" ? [] : cardChildren([node.expression]);
      case "ConditionalExpression":
        return cardChildren([node.consequent, node.alternate]);
      case "LogicalExpression":
        return cardChildren([node.right]);
      default:
        return [];
    }
  });
}

function elementName(element: ESTree.JSXElement): string | null {
  const name = element.openingElement.name;
  return name.type === "JSXIdentifier" ? name.name : null;
}

/** A section's card draws one hairline per child, so each child must be a row primitive that carries its archetype. */
const sectionChildrenArePrimitives = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Require every child of a SettingsSection card to be a settings primitive." },
    messages: {
      intrinsic:
        "`<{{name}}>` is a hand-built row inside a SettingsSection. Use a settings primitive (SettingsRow, SettingsList, SettingsFoot, SettingsDanger, …) so the row gets its archetype and hairline.",
      legacy:
        "`<{{name}}>` is the old row shape. Inside a SettingsSection, use SettingsRow or another settings primitive instead.",
    },
  },
  createOnce(context) {
    return {
      JSXElement(node) {
        if (elementName(node) !== "SettingsSection") return;
        for (const child of cardChildren(node.children)) {
          const name = elementName(child);
          if (name === null) continue;
          if (/^[a-z]/u.test(name)) context.report({ node: child, messageId: "intrinsic", data: { name } });
          else if (LEGACY_ROWS.has(name)) context.report({ node: child, messageId: "legacy", data: { name } });
        }
      },
    };
  },
});

/** The old field shape: a label stacked on a control, with its own spacing. Never right anywhere in a settings card. */
const RAW_FIELDS = new Set(["Field", "FieldGroup", "FieldSet", "FieldLabel", "FieldContent", "FieldDescription", "FieldTitle"]);
/** Bare controls. Right only inside a primitive that lays them out and labels them. */
const RAW_INPUTS = new Set(["Input", "Textarea", "InputGroup"]);
/** Primitives built to hold a control: inside one, a bare control is laid out and labelled for the card. */
const CONTROL_HOMES = new Set(["SettingsRow", "SettingsField", "SettingsForm", "SettingsFormField", "SettingsListItem", "SettingsWorkspace"]);
/** Surfaces that float above the card and draw their own layout, so the card's rules stop at them. */
const SURFACES = new Set(["Dialog", "DialogContent", "AlertDialog", "AlertDialogContent", "Sheet", "SheetContent", "Popover", "PopoverContent", "DropdownMenuContent"]);

type Body = ESTree.Node;

function isNode(value: unknown): value is ESTree.Node {
  return typeof value === "object" && value !== null && typeof (value as { type?: unknown }).type === "string";
}

/** A file the rule can see into: its own components and helpers by name, and where each imported name comes from. */
interface Scope {
  file: string;
  bodies: Map<string, Body>;
  values: Map<string, ESTree.Node[]>;
  imports: Map<string, { source: string; name: string }>;
}

function functionBody(node: ESTree.Node): Body | null {
  if (node.type === "FunctionDeclaration") return node.body ?? node;
  if (node.type === "ArrowFunctionExpression" || node.type === "FunctionExpression") return node.body;
  return null;
}

/** The file's own components and helpers by exported and local name, so a section that renders `<Rows />` is checked through `Rows`. */
function scopeOf(file: string, program: ESTree.Program): Scope {
  const bodies = new Map<string, Body>();
  const imports = new Map<string, { source: string; name: string }>();
  const aliases: [string, string][] = [];
  const visit = (statement: ESTree.Node) => {
    if (statement.type === "ImportDeclaration") {
      if (statement.importKind === "type") return;
      for (const specifier of statement.specifiers) {
        if (specifier.type === "ImportNamespaceSpecifier") continue;
        const name = specifier.type === "ImportDefaultSpecifier" ? "default"
          : specifier.imported.type === "Identifier" ? specifier.imported.name : String(specifier.imported.value);
        imports.set(specifier.local.name, { source: String(statement.source.value), name });
      }
      return;
    }
    if (statement.type === "ExportNamedDeclaration") {
      if (statement.declaration && isNode(statement.declaration)) visit(statement.declaration);
      for (const specifier of statement.specifiers) {
        const local = specifier.local.type === "Identifier" ? specifier.local.name : String(specifier.local.value);
        const exported = specifier.exported.type === "Identifier" ? specifier.exported.name : String(specifier.exported.value);
        if (statement.source) imports.set(exported, { source: String(statement.source.value), name: local });
        else aliases.push([exported, local]);
      }
      return;
    }
    if (statement.type === "ExportDefaultDeclaration") {
      const declaration = statement.declaration;
      const body = isNode(declaration) ? functionBody(declaration) : null;
      if (body) bodies.set("default", body);
      if (declaration.type === "Identifier") aliases.push(["default", declaration.name]);
      if (isNode(declaration)) visit(declaration);
      return;
    }
    if (statement.type === "FunctionDeclaration" && statement.id) bodies.set(statement.id.name, statement.body ?? statement);
    if (statement.type === "VariableDeclaration") {
      for (const declarator of statement.declarations) {
        const body = declarator.init ? functionBody(declarator.init) : null;
        if (declarator.id.type === "Identifier" && body) bodies.set(declarator.id.name, body);
      }
    }
  };
  for (const statement of program.body) visit(statement);
  const values = new Map<string, ESTree.Node[]>();
  const collect = (node: ESTree.Node): void => {
    if (node.type === "VariableDeclarator" && node.id.type === "Identifier" && node.init) {
      values.set(node.id.name, [...values.get(node.id.name) ?? [], node.init]);
    }
    for (const [key, value] of Object.entries(node)) {
      if (key === "parent") continue;
      if (Array.isArray(value)) {
        for (const item of value) if (isNode(item)) collect(item);
      } else if (isNode(value)) collect(value);
    }
  };
  collect(program);
  for (const [exported, local] of aliases) {
    const body = bodies.get(local);
    if (body && !bodies.has(exported)) bodies.set(exported, body);
  }
  return { file, bodies, values, imports };
}

/** A child that names something rather than refers to a value: a declared binding, a key, a non-computed member. */
function isName(node: ESTree.Node, key: string): boolean {
  if (node.type === "VariableDeclarator") return key === "id";
  if (node.type === "Property" || node.type === "PropertyDefinition") return key === "key" && !node.computed;
  if (node.type === "MemberExpression") return key === "property" && !node.computed;
  return false;
}

const RESOLVE_SUFFIXES = ["", ".tsx", ".ts", "/index.tsx", "/index.ts"];

/** The app file an import names, or null for packages and for the shared primitives, which own their own layout. */
function resolveImport(srcRoot: string, from: string, source: string): string | null {
  const base = source.startsWith("#/") ? path.join(srcRoot, source.slice(2))
    : source.startsWith(".") ? path.resolve(path.dirname(from), source)
    : null;
  if (base === null || base.startsWith(path.join(srcRoot, "components") + path.sep)) return null;
  for (const suffix of RESOLVE_SUFFIXES) {
    const file = base + suffix;
    if (/\.tsx?$/u.test(file) && fs.existsSync(file) && fs.statSync(file).isFile()) return file;
  }
  return null;
}

const importedScopes = new Map<string, Scope | null>();

function importedScope(file: string): Scope | null {
  if (!importedScopes.has(file)) {
    const parsed = parseSync(file, fs.readFileSync(file, "utf8"));
    // SAFETY: oxc-parser and oxlint share one ESTree shape; the plugin types just name it separately.
    importedScopes.set(file, parsed.errors.length > 0 ? null : scopeOf(file, parsed.program as unknown as ESTree.Program));
  }
  return importedScopes.get(file) ?? null;
}

/**
 * A raw Field anywhere under a SettingsSection, or a raw Input outside a primitive built to hold it, draws its own
 * label and spacing inside the card. Follows the file's own components and helpers and the app components it imports,
 * reporting an imported one at the element in this file that renders it, and stops at floating surfaces.
 */
const noRawFieldInSection = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Disallow raw Field and unhoused Input anywhere under a SettingsSection." },
    messages: {
      field: "`<{{name}}>` is the old field shape inside a SettingsSection. Use SettingsRow, SettingsField or SettingsFormField.",
      input: "`<{{name}}>` sits bare inside a SettingsSection. Put it in a SettingsRow, SettingsField or SettingsForm, or use ConfirmableInput there.",
      fieldVia: "`<{{via}}>` renders `<{{name}}>` ({{file}}), the old field shape, inside a SettingsSection. Use SettingsRow, SettingsField or SettingsFormField there.",
      inputVia: "`<{{via}}>` renders `<{{name}}>` ({{file}}) bare inside a SettingsSection. Put it in a SettingsRow, SettingsField or SettingsForm, or use ConfirmableInput there.",
    },
  },
  createOnce(context) {
    return {
      Program(program) {
        const srcRoot = path.join(context.cwd, "src");
        const home = scopeOf(context.filename, program);
        const reported = new Set<string>();
        const report = (node: ESTree.JSXElement, entry: ESTree.JSXElement | null, scope: Scope, kind: "field" | "input", name: string) => {
          const at = entry ?? node;
          const key = `${at.start}:${scope.file}:${node.start}`;
          if (reported.has(key)) return;
          reported.add(key);
          if (entry === null) context.report({ node, messageId: kind, data: { name } });
          else context.report({ node: entry, messageId: kind === "field" ? "fieldVia" : "inputVia", data: { name, via: elementName(entry) ?? "", file: path.relative(context.cwd, scope.file) } });
        };
        const followed = new Set<string>();
        const follow = (name: string, housed: boolean, scope: Scope, entry: ESTree.JSXElement | null, site: ESTree.Node) => {
          let target: Scope | null = scope;
          let local = name;
          while (target && !target.bodies.has(local)) {
            const origin: { source: string; name: string } | undefined = target.imports.get(local);
            const file = origin ? resolveImport(srcRoot, target.file, origin.source) : null;
            target = file ? importedScope(file) : null;
            local = origin?.name ?? local;
          }
          const body = target?.bodies.get(local);
          if (!target || !body) return;
          const via = target === scope ? entry : entry ?? (site.type === "JSXElement" ? site : null);
          if (target !== scope && via === null) return;
          const key = `${via?.start ?? -1}:${target.file}:${local}:${housed}`;
          if (followed.has(key)) return;
          followed.add(key);
          walk(body, housed, target, via);
        };
        const walk = (node: ESTree.Node, housed: boolean, scope: Scope, entry: ESTree.JSXElement | null): void => {
          let inside = housed;
          if (node.type === "JSXElement") {
            const name = elementName(node);
            if (name !== null) {
              if (SURFACES.has(name)) return;
              if (RAW_FIELDS.has(name)) report(node, entry, scope, "field", name);
              else if (RAW_INPUTS.has(name) && !housed) report(node, entry, scope, "input", name);
              if (CONTROL_HOMES.has(name)) inside = true;
              follow(name, inside, scope, entry, node);
            }
          }
          if (node.type === "CallExpression" && node.callee.type === "Identifier") follow(node.callee.name, inside, scope, entry, node);
          if (node.type === "Identifier") {
            for (const value of scope.values.get(node.name) ?? []) {
              const key = `${entry?.start ?? -1}:${scope.file}:${value.start}:${inside}`;
              if (followed.has(key)) continue;
              followed.add(key);
              walk(value, inside, scope, entry);
            }
          }
          for (const [key, value] of Object.entries(node)) {
            if (key === "parent" || isName(node, key)) continue;
            if (Array.isArray(value)) {
              for (const item of value) if (isNode(item)) walk(item, inside, scope, entry);
            } else if (isNode(value)) walk(value, inside, scope, entry);
          }
        };
        const findSections = (node: ESTree.Node): void => {
          if (node.type === "JSXElement" && elementName(node) === "SettingsSection") {
            for (const child of node.children) walk(child, false, home, null);
            for (const attribute of node.openingElement.attributes) walk(attribute, false, home, null);
          }
          for (const [key, value] of Object.entries(node)) {
            if (key === "parent") continue;
            if (Array.isArray(value)) {
              for (const item of value) if (isNode(item)) findSections(item);
            } else if (isNode(value)) findSections(value);
          }
        };
        findSections(program);
      },
    };
  },
});

/** Visits each class token in the file's string literals and template literals' static parts. */
function onClassTokens(report: (node: ESTree.Node, token: string) => void) {
  const check = (node: ESTree.Node, text: string) => {
    for (const token of text.split(/\s+/u)) if (token) report(node, token);
  };
  return {
    Literal(node: ESTree.Literal) {
      if (typeof node.value === "string") check(node, node.value);
    },
    TemplateLiteral(node: ESTree.TemplateLiteral) {
      check(node, node.quasis.map((quasi) => quasi.value.cooked ?? "").join(" "));
    },
  };
}

/** Staged pink belongs to the control: `data-changed` on it draws the pink. Painting it by hand drifts from the rest. */
const noHandDrawnStaged = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Disallow staged pink classes that aren't keyed on data-changed." },
    messages: {
      pink: "`{{token}}` paints staged pink by hand. Put `data-changed` on the control instead, or key the class on `data-[changed=true]:`.",
    },
  },
  createOnce(context) {
    return onClassTokens((node, token) => {
      if (/(?:^|:)(?:bg|border|ring|outline)-changed-(?:border|soft)\b/u.test(token) && !token.includes("data-[changed=true]")) {
        context.report({ node, messageId: "pink", data: { token } });
      }
    });
  },
});

/** A staged removal is a row's `tone="removed"`, so every struck row looks the same. */
const noHandDrawnTone = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Disallow line-through in files built from settings primitives." },
    messages: {
      tone: "`line-through` strikes a row by hand. Pass `tone=\"removed\"` to the settings primitive instead.",
    },
  },
  createOnce(context) {
    let settings = false;
    const strings = onClassTokens((node, token) => {
      if (settings && token.endsWith("line-through")) context.report({ node, messageId: "tone" });
    });
    return {
      Program(program) {
        settings = program.body.some((statement) => statement.type === "ImportDeclaration"
          && String(statement.source.value).startsWith("#/components/settings/"));
      },
      ...strings,
    };
  },
});

/** Rules that hold every settings surface to one row language. */
const settingsRowsPlugin = eslintCompatPlugin({
  meta: { name: "settings-rows" },
  rules: {
    "section-children-are-primitives": sectionChildrenArePrimitives,
    "no-hand-drawn-staged": noHandDrawnStaged,
    "no-hand-drawn-tone": noHandDrawnTone,
    "no-raw-field-in-section": noRawFieldInSection,
  },
});

export default settingsRowsPlugin;
