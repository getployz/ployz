import { defineRule, eslintCompatPlugin } from "@oxlint/plugins";
import type { ESTree } from "@oxlint/plugins";

const OFF_SCALE_CLASS = /(?:^|[\s:"'`])text-(?:lg|xl|[3-9]xl|\[\d[^\]]*\])(?=$|[\s"'`])/;
const ON_SCALE_VALUE = /^var\(--text-(?:xs|sm|base|2xl)\)$/;

function staticText(node: ESTree.Node): string | null {
  if (node.type === "Literal") return typeof node.value === "string" ? node.value : null;
  if (node.type === "TemplateLiteral") return node.quasis.map((quasi) => quasi.value.cooked ?? quasi.value.raw).join(" ");
  return null;
}

/** DESIGN.md rule 3: text is 12, 14, 16 or 24px, through `text-xs`, `text-sm`, `text-base` and `text-2xl`. */
const typeScaleRule = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Keep text on the 12, 14, 16 and 24px scale (dashboard/DESIGN.md rule 3)." },
    messages: {
      offScaleClass: "Use `text-xs`, `text-sm`, `text-base` or `text-2xl`. The type scale is 12, 14, 16 and 24px.",
      offScaleStyle: "Set `fontSize` to `var(--text-xs)`, `var(--text-sm)`, `var(--text-base)` or `var(--text-2xl)`, or use the matching class.",
    },
  },
  createOnce(context) {
    const checkText = (node: ESTree.Node) => {
      const text = staticText(node);
      if (text !== null && OFF_SCALE_CLASS.test(text)) context.report({ node, messageId: "offScaleClass" });
    };
    return {
      Literal: checkText,
      TemplateLiteral: checkText,
      Property(node) {
        const key = node.key.type === "Identifier" ? node.key.name : node.key.type === "Literal" ? node.key.value : null;
        if (key !== "fontSize" && key !== "font-size") return;
        const value = staticText(node.value);
        if (value === null || !ON_SCALE_VALUE.test(value)) context.report({ node, messageId: "offScaleStyle" });
      },
    };
  },
});

export default eslintCompatPlugin({ meta: { name: "design" }, rules: { "type-scale": typeScaleRule } });
