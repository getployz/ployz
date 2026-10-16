import { defineRule, eslintCompatPlugin } from "@oxlint/plugins";
import type { ESTree } from "@oxlint/plugins";

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

/** The file's own components and helpers by name, so a section that renders `<Rows />` is checked through `Rows`. */
function localBodies(program: ESTree.Program): Map<string, Body> {
  const bodies = new Map<string, Body>();
  const visit = (statement: ESTree.Node) => {
    if (statement.type === "ExportNamedDeclaration" || statement.type === "ExportDefaultDeclaration") {
      if (statement.declaration && isNode(statement.declaration)) visit(statement.declaration);
      return;
    }
    if (statement.type === "FunctionDeclaration" && statement.id) bodies.set(statement.id.name, statement.body ?? statement);
    if (statement.type === "VariableDeclaration") {
      for (const declarator of statement.declarations) {
        const init = declarator.init;
        if (declarator.id.type === "Identifier" && init && (init.type === "ArrowFunctionExpression" || init.type === "FunctionExpression")) {
          bodies.set(declarator.id.name, init.body);
        }
      }
    }
  };
  for (const statement of program.body) visit(statement);
  return bodies;
}

/**
 * A raw Field anywhere under a SettingsSection, or a raw Input outside a primitive built to hold it, draws its own
 * label and spacing inside the card. Follows the file's own components and helpers, and stops at floating surfaces.
 */
const noRawFieldInSection = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Disallow raw Field and unhoused Input anywhere under a SettingsSection." },
    messages: {
      field: "`<{{name}}>` is the old field shape inside a SettingsSection. Use SettingsRow, SettingsField or SettingsFormField.",
      input: "`<{{name}}>` sits bare inside a SettingsSection. Put it in a SettingsRow, SettingsField or SettingsForm, or use ConfirmableInput there.",
    },
  },
  createOnce(context) {
    return {
      Program(program) {
        const bodies = localBodies(program);
        const reported = new Set<ESTree.Node>();
        const report = (node: ESTree.JSXElement, messageId: "field" | "input", name: string) => {
          if (reported.has(node)) return;
          reported.add(node);
          context.report({ node, messageId, data: { name } });
        };
        const followed = new Set<string>();
        const follow = (name: string, housed: boolean) => {
          const body = bodies.get(name);
          const key = `${name}:${housed}`;
          if (!body || followed.has(key)) return;
          followed.add(key);
          walk(body, housed);
        };
        const walk = (node: ESTree.Node, housed: boolean): void => {
          let inside = housed;
          if (node.type === "JSXElement") {
            const name = elementName(node);
            if (name !== null) {
              if (SURFACES.has(name)) return;
              if (RAW_FIELDS.has(name)) report(node, "field", name);
              else if (RAW_INPUTS.has(name) && !housed) report(node, "input", name);
              if (CONTROL_HOMES.has(name)) inside = true;
              follow(name, inside);
            }
          }
          if (node.type === "CallExpression" && node.callee.type === "Identifier") follow(node.callee.name, inside);
          for (const [key, value] of Object.entries(node)) {
            if (key === "parent") continue;
            if (Array.isArray(value)) {
              for (const item of value) if (isNode(item)) walk(item, inside);
            } else if (isNode(value)) walk(value, inside);
          }
        };
        const findSections = (node: ESTree.Node): void => {
          if (node.type === "JSXElement" && elementName(node) === "SettingsSection") {
            for (const child of node.children) walk(child, false);
            for (const attribute of node.openingElement.attributes) walk(attribute, false);
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
