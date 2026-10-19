import { createContext, useContext, useRef, useState } from "react"

const OpenerFocusContext = createContext<HTMLElement | null>(null)

const focused = () => {
  if (import.meta.env.SSR) return null
  const active = document.activeElement
  return active instanceof HTMLElement && active !== document.body ? active : null
}

/**
 * Remembers what had focus as a dialog opens, during render: Base UI notes it only after an `autoFocus` field has
 * taken focus, so a dialog with no Trigger had nowhere to send it back.
 */
function useOpenerFocus(open: boolean | undefined) {
  const [state, setState] = useState(() => ({ open, opener: open ? focused() : null }))
  if (state.open !== open) setState({ open, opener: open ? focused() : state.opener })
  return state.opener
}

/**
 * Sends focus back to the opener only when closing dropped it, onto the body or the closed popup. Base UI picks its
 * target as the popup closes but focuses a microtask later, so this checks then: by that time focus may have moved
 * on, such as into the next dialog's field, and stays there.
 */
function useOpenerFinalFocus() {
  const opener = useContext(OpenerFocusContext)
  const popup = useRef<HTMLDivElement>(null)
  if (!opener) return { popup, finalFocus: undefined }
  const finalFocus = () => {
    if (!opener.isConnected) return true
    queueMicrotask(() => {
      const active = document.activeElement
      if (!active || active === document.body || popup.current?.contains(active)) opener.focus({ preventScroll: true })
    })
    return false
  }
  return { popup, finalFocus }
}

export { OpenerFocusContext, useOpenerFinalFocus, useOpenerFocus }
