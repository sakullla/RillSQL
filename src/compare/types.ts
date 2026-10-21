export interface CompareSide {
  connectionId: string;
  database: string;
  schema: string;
  engine: string;
}

export interface DiffItem {
  kind: string;
  name: string;
  detail: string;
}

export interface KeyChoice {
  name: string;
  columns: string[];
  primary: boolean;
}

export interface TableChoice {
  name: string;
  onBoth: boolean;
  needsKeyChoice: boolean;
  keys: KeyChoice[];
}

export interface StructureReport {
  refused: string | null;
  script: string;
  incomplete: boolean;
  notes: string[];
  onlyBaseline: DiffItem[];
  onlyTarget: DiffItem[];
  changed: DiffItem[];
  tables: TableChoice[];
}

export interface DataDiff {
  table: string;
  action: string;
  key: string;
  detail: string;
}

export interface DataReport {
  refused: string | null;
  script: string;
  incomplete: boolean;
  notes: string[];
  diffs: DataDiff[];
}

export const EMPTY_STRUCTURE: StructureReport = {
  refused: null,
  script: "",
  incomplete: false,
  notes: [],
  onlyBaseline: [],
  onlyTarget: [],
  changed: [],
  tables: [],
};
