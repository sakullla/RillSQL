export const MODULE_ID = "catalog";

export interface PaletteTable {
  id: string;
  label: string;
  path: string;
}

export function listPaletteTables(): PaletteTable[] {
  return [];
}
