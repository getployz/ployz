import { createContext, useContext, useState } from "react"

type FinalFocus = () => HTMLElement | true

const OpenerFocusContext = createContext<FinalFocus | undefined>(undefined)

const focused = () =>
  !import.meta.env.SSR && document.activeElement instanceof HTMLElement ? document.activeElement : null

/**
 * Remembers what had focus as a dialog opens, during render: Base UI notes it only after an `autoFocus` field has
 * taken focus, so a dialog with no Trigger had nowhere to send it back.
 */
function useOpenerFocus(open: boolean | undefined) {
  const [state, setState] = useState(() => ({ open, opener: open ? focused() : null }))
  if (state.open !== open) setState({ open, opener: open ? focused() : state.opener })
  const { opener } = state
  return opener ? () => (opener.isConnected ? opener : true) : undefined
}

const useOpenerFinalFocus = () => useContext(OpenerFocusContext)

export { OpenerFocusContext, useOpenerFinalFocus, useOpenerFocus }
