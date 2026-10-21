import { createContext, use } from "react";

/** Which template's Services fill the canvas; null while each template is one node. */
export const TemplateView = createContext<{ open: string | null; setOpen: (id: string | null) => void }>({ open: null, setOpen: () => {} });

export const useTemplateView = () => use(TemplateView);
