export type VariableRowState = {
  editing: boolean;
  editValue: string;
  /** An edit just closed, so focus goes back to the row's menu instead of the page. */
  refocusActions: boolean;
  revealed: boolean;
  confirmSealOpen: boolean;
  renameOpen: boolean;
};

export type VariableRowAction =
  | { type: "editOpened"; value: string }
  | { type: "editCancelled" }
  | { type: "editValueChanged"; value: string }
  | { type: "revealToggled" }
  | { type: "saveSucceeded" }
  | { type: "sealDialogChanged"; open: boolean }
  | { type: "renameDialogChanged"; open: boolean };

export const initialVariableRowState: VariableRowState = {
  editing: false,
  editValue: "",
  refocusActions: false,
  revealed: false,
  confirmSealOpen: false,
  renameOpen: false,
};

export function variableRowReducer(
  state: VariableRowState,
  action: VariableRowAction,
): VariableRowState {
  switch (action.type) {
    case "editOpened":
      return { ...state, editing: true, editValue: action.value, refocusActions: false };
    case "editCancelled":
      return { ...state, editing: false, editValue: "", refocusActions: true };
    case "editValueChanged":
      return { ...state, editValue: action.value };
    case "revealToggled":
      return { ...state, revealed: !state.revealed };
    case "saveSucceeded":
      return { ...state, editing: false, editValue: "", refocusActions: true };
    case "sealDialogChanged":
      return { ...state, confirmSealOpen: action.open };
    case "renameDialogChanged":
      return { ...state, renameOpen: action.open };
  }
}
