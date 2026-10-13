export const APP_PAGES = ["workspace", "connections", "history", "settings"] as const;

export type AppPage = (typeof APP_PAGES)[number];

const PAGE_SET = new Set<string>(APP_PAGES);

export function parseHash(hash: string): AppPage {
  const raw = hash.replace(/^#\/?/, "").split(/[?#]/)[0] ?? "";
  if (raw === "workspace" || raw === "") return "workspace";
  if (PAGE_SET.has(raw)) return raw as AppPage;
  return "workspace";
}

export function toHash(page: AppPage): string {
  return page === "workspace" ? "#/" : `#/${page}`;
}
