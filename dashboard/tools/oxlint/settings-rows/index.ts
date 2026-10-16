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
  },
});

export default settingsRowsPlugin;
