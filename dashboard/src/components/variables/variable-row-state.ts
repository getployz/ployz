export type VariableRowState = {
  editing: boolean;
  editValue: string;
  revealed: boolean;
  confirmSealOpen: boolean;
  confirmDeleteOpen: boolean;
};

export type VariableRowAction =
  | { type: "editOpened"; value: string }
  | { type: "editCancelled" }
  | { type: "editValueChanged"; value: string }
  | { type: "revealToggled" }
  | { type: "saveSucceeded" }
  | { type: "sealDialogChanged"; open: boolean }
  | { type: "deleteDialogChanged"; open: boolean };

export const initialVariableRowState: VariableRowState = {
  editing: false,
  editValue: "",
  revealed: false,
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
    case "saveSucceeded":
      return { ...state, editing: false, editValue: "" };
    case "sealDialogChanged":
      return { ...state, confirmSealOpen: action.open };
    case "deleteDialogChanged":
      return { ...state, confirmDeleteOpen: action.open };
  }
}
