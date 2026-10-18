export interface ListboxOption {
  value: string;
  label: string;
  hint?: string;
  disabled?: boolean;
}

export interface SegmentedOption {
  value: string;
  label: string;
  disabled?: boolean;
}

export type MenuItem =
  | { kind: "item"; id: string; label: string; shortcut?: string; title?: string; danger?: boolean; disabled?: boolean; checked?: boolean }
  | { kind: "head"; text: string }
  | { kind: "sep" };
