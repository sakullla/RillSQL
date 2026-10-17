import type { MenuItem } from "../ui";

export interface MenuDefinition {
  id: string;
  label: string;
  items: MenuItem[];
}
