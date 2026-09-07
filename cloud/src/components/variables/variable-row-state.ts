export type VariableRowState = {
  editing: boolean;
  editValue: string;
  revealed: boolean;
  isSaving: boolean;
  confirmSealOpen: boolean;
  confirmDeleteOpen: boolean;
};

export type VariableRowAction =
  | { type: "editOpened"; value: string }
  | { type: "editCancelled" }
  | { type: "editValueChanged"; value: string }
  | { type: "revealToggled" }
  | { type: "saveStarted" }
  | { type: "saveSucceeded" }
  | { type: "saveFailed" }
  | { type: "sealDialogChanged"; open: boolean }
  | { type: "deleteDialogChanged"; open: boolean };

export const initialVariableRowState: VariableRowState = {
  editing: false,
  editValue: "",
  revealed: false,
  isSaving: false,
  confirmSealOpen: false,
  confirmDeleteOpen: false,
};

export function variableRowReducer(
  state: VariableRowState,
  action: VariableRowAction,
): VariableRowState {
  switch (action.type) {
    case "editOpened":
      return { ...state, editing: true, editValue: action.value };
    case "editCancelled":
      return { ...state, editing: false, editValue: "" };
    case "editValueChanged":
      return { ...state, editValue: action.value };
    case "revealToggled":
      return { ...state, revealed: !state.revealed };
    case "saveStarted":
      return { ...state, isSaving: true };
    case "saveSucceeded":
      return { ...state, editing: false, editValue: "", isSaving: false };
    case "saveFailed":
      return { ...state, isSaving: false };
    case "sealDialogChanged":
      return { ...state, confirmSealOpen: action.open };
    case "deleteDialogChanged":
      return { ...state, confirmDeleteOpen: action.open };
  }
}
