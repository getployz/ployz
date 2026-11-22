import { useEffect, useState } from "react";

type ConfigFileEditor = typeof import("./ConfigFileEditor").default;

let loaded: ConfigFileEditor | null = null;

/**
 * The editor once its chunk is in, else null. Not `lazy`: a suspended drawer holds its fallback for React's 300 ms
 * reveal throttle, and that wait was most of a Config drawer's first open.
 */
export function useConfigFileEditor() {
  const [editor, setEditor] = useState<ConfigFileEditor | null>(() => loaded);
  useEffect(() => {
    if (!editor) void import("./ConfigFileEditor").then((module) => setEditor(() => (loaded = module.default)));
  }, [editor]);
  return editor;
}
