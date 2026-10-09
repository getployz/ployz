import { autocompletion, completionKeymap, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { json } from "@codemirror/lang-json";
import { yaml } from "@codemirror/lang-yaml";
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { Compartment, EditorState, Facet, RangeSetBuilder, type Extension } from "@codemirror/state";
import {
  Decoration, type DecorationSet, EditorView, hoverTooltip, keymap, lineNumbers, tooltips, ViewPlugin, type ViewUpdate,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { useEffect, useRef } from "react";
import { configReferences, type ConfigReference } from "#/modules/config-store/config-references";
import { filterReferenceTargets, type ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import { buildRefToken, caretToken } from "#/modules/variables/variable-template";

export type ConfigFileEditorProps = {
  fileName: string;
  value: string;
  onChange: (value: string) => void;
  onSave: () => void;
  targets: readonly ReferenceTarget[];
  services: readonly string[];
  ariaLabel: string;
};

type ReferenceContext = { targets: readonly ReferenceTarget[]; services: readonly string[] };

const referenceContext = Facet.define<ReferenceContext, ReferenceContext>({
  combine: (values) => values[0] ?? { targets: [], services: [] },
});

const pill = Decoration.mark({ class: "cm-config-ref" });
const unknown = Decoration.mark({ class: "cm-config-unknown" });

const referenceDecorations = ViewPlugin.fromClass(class {
  decorations: DecorationSet;
  references: ConfigReference[] = [];

  constructor(view: EditorView) {
    this.decorations = this.scan(view);
  }

  update(update: ViewUpdate) {
    if (update.docChanged || update.viewportChanged
      || update.startState.facet(referenceContext) !== update.state.facet(referenceContext)) {
      this.decorations = this.scan(update.view);
    }
  }

  scan(view: EditorView): DecorationSet {
    const { targets, services } = view.state.facet(referenceContext);
    const { doc } = view.state;
    const builder = new RangeSetBuilder<Decoration>();
    this.references = [];
    for (const range of view.visibleRanges) {
      const from = doc.lineAt(range.from).from;
      const to = doc.lineAt(range.to).to;
      for (const reference of configReferences(doc.sliceString(from, to), targets, services)) {
        const placed = { ...reference, from: from + reference.from, to: from + reference.to };
        this.references.push(placed);
        builder.add(placed.from, placed.to, placed.kind === "unknown" ? unknown : pill);
      }
    }
    return builder.finish();
  }
}, { decorations: (plugin) => plugin.decorations });

const unknownTooltip = hoverTooltip((view, pos) => {
  const reference = view.plugin(referenceDecorations)?.references
    .find((candidate) => candidate.kind === "unknown" && candidate.from <= pos && pos <= candidate.to);
  if (reference?.kind !== "unknown") return null;
  return {
    pos: reference.from,
    end: reference.to,
    above: true,
    create: () => {
      const dom = document.createElement("div");
      dom.className = "cm-config-tooltip";
      dom.textContent = reference.message;
      return { dom };
    },
  };
});

function referenceCompletions(context: CompletionContext): CompletionResult | null {
  const line = context.state.doc.lineAt(context.pos);
  const token = caretToken(line.text.slice(0, context.pos - line.from), context.pos - line.from);
  if (token === null) return null;
  const { targets } = context.state.facet(referenceContext);
  const serviceTargets = targets.filter((target) => target.ownerSlug !== null);
  return {
    from: line.from + token.start,
    filter: false,
    options: filterReferenceTargets(serviceTargets, token).map((target) => ({
      label: `${target.ownerSlug}.${target.key}`,
      detail: target.ownerLabel,
      apply: (view, _completion, from, to) => {
        const insert = buildRefToken({ ownerSlug: target.ownerSlug, key: target.key });
        const closing = /^\s*\}\}/.exec(view.state.sliceDoc(to, to + 64))?.[0].length ?? 0;
        view.dispatch({
          changes: { from, to: to + closing, insert },
          selection: { anchor: from + insert.length },
          userEvent: "input.complete",
        });
      },
    })),
  };
}

const highlight = HighlightStyle.define([
  { tag: [tags.propertyName, tags.definition(tags.propertyName), tags.keyword], color: "var(--chart-1)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--chart-3)" },
  { tag: [tags.number, tags.bool, tags.null, tags.atom], color: "var(--chart-4)" },
  { tag: [tags.comment, tags.meta], color: "var(--muted-foreground)", fontStyle: "italic" },
  { tag: [tags.function(tags.variableName), tags.className], color: "var(--chart-2)" },
]);

const theme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--background)", color: "var(--foreground)", fontSize: "13px" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: "var(--font-mono)", lineHeight: "1.6" },
  ".cm-content": { caretColor: "var(--foreground)" },
  ".cm-cursor": { borderLeftColor: "var(--foreground)" },
  ".cm-gutters": { backgroundColor: "var(--background)", color: "var(--muted-foreground)", border: "none" },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 12px 0 16px" },
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "transparent" },
  "&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "color-mix(in srgb, var(--chart-1) 25%, transparent)",
  },
  ".cm-config-ref": { backgroundColor: "var(--muted)", borderRadius: "4px", padding: "1px 4px" },
  ".cm-config-unknown": { textDecoration: "underline wavy var(--destructive)", textUnderlineOffset: "3px" },
  ".cm-tooltip": {
    backgroundColor: "var(--popover)", color: "var(--popover-foreground)",
    border: "1px solid var(--border)", borderRadius: "6px", overflow: "hidden",
  },
  ".cm-config-tooltip": { padding: "4px 8px", fontFamily: "var(--font-sans)", fontSize: "12px" },
  ".cm-tooltip-autocomplete > ul": { fontFamily: "var(--font-mono)", fontSize: "12px" },
  ".cm-tooltip-autocomplete > ul > li": { padding: "2px 8px" },
  ".cm-tooltip-autocomplete > ul > li[aria-selected]": { backgroundColor: "var(--muted)", color: "var(--foreground)" },
  ".cm-completionDetail": { marginLeft: "12px", fontStyle: "normal", color: "var(--muted-foreground)" },
});

const plain: Extension = [];
// Compartments are keys into each editor's own state, so every editor can share them.
const languageSlot = new Compartment();
const propsSlot = new Compartment();

function language(fileName: string): Extension | Promise<Extension> {
  const extension = /\.([^.]+)$/u.exec(fileName)?.[1]?.toLowerCase();
  switch (extension) {
    case "yml":
    case "yaml":
      return yaml();
    case "json":
      return json();
    case "toml":
      return StreamLanguage.define(toml);
    case "py":
      return import("@codemirror/lang-python").then(({ python }) => python());
    default:
      return plain;
  }
}

export default function ConfigFileEditor({ fileName, value, onChange, onSave, targets, services, ariaLabel }: ConfigFileEditorProps) {
  const parent = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const callbacks = useRef({ onChange, onSave });
  const editorText = useRef(value);

  useEffect(() => {
    callbacks.current = { onChange, onSave };
  });

  const props = (): Extension => [
    referenceContext.of({ targets, services }),
    EditorView.contentAttributes.of({ "aria-label": ariaLabel }),
  ];

  const state = (doc: string, current?: EditorState) => EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      history(),
      keymap.of([
        {
          key: "Mod-s",
          preventDefault: true,
          run: () => {
            callbacks.current.onSave();
            return true;
          },
        },
        ...completionKeymap,
        ...defaultKeymap,
        ...historyKeymap,
        indentWithTab,
      ]),
      // On the body, so the drawer's scroll box doesn't clip the list.
      tooltips({ parent: document.body }),
      autocompletion({ override: [referenceCompletions], icons: false }),
      referenceDecorations,
      unknownTooltip,
      syntaxHighlighting(highlight),
      theme,
      languageSlot.of(current ? languageSlot.get(current) ?? plain : plain),
      propsSlot.of(props()),
      EditorView.updateListener.of((update) => {
        if (!update.docChanged) return;
        editorText.current = update.state.doc.toString();
        callbacks.current.onChange(editorText.current);
      }),
    ],
  });

  useEffect(() => {
    if (!parent.current) return;
    const editor = new EditorView({ parent: parent.current, state: state(editorText.current) });
    view.current = editor;
    return () => {
      editor.destroy();
      view.current = null;
    };
  }, []);

  useEffect(() => {
    const editor = view.current;
    if (!editor || value === editorText.current) return;
    editorText.current = value;
    // A fresh state, so undo never crosses into another file's text.
    editor.setState(state(value, editor.state));
  });

  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    let current = true;
    const loaded = language(fileName);
    if (loaded instanceof Promise) {
      void loaded.then((extension) => {
        if (current) editor.dispatch({ effects: languageSlot.reconfigure(extension) });
      });
    } else {
      editor.dispatch({ effects: languageSlot.reconfigure(loaded) });
    }
    return () => {
      current = false;
    };
  }, [fileName]);

  useEffect(() => {
    view.current?.dispatch({ effects: propsSlot.reconfigure(props()) });
  }, [targets, services, ariaLabel]);

  return <div ref={parent} className="ph-no-capture h-full min-h-0 overflow-hidden" />;
}
