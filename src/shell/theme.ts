export type ThemePreference = "light" | "dark" | "system";
export type Density = "compact" | "standard" | "relaxed";
export type ResolvedTheme = "light" | "dark";

export function resolvedTheme(preference: ThemePreference, prefersDark: boolean): ResolvedTheme {
  if (preference === "system") return prefersDark ? "dark" : "light";
  return preference;
}

export function applyDocumentTheme(
  preference: ThemePreference,
  density: Density,
  prefersDark: boolean,
  editorFont: number,
  root: HTMLElement = document.documentElement,
): ResolvedTheme {
  const theme = resolvedTheme(preference, prefersDark);
  root.dataset.theme = theme;
  root.dataset.density = density;
  root.style.setProperty("--editor-font", `${editorFont}px`);
  return theme;
}

export function prefersDarkScheme(query = "(prefers-color-scheme: dark)"): boolean {
  if (typeof window.matchMedia !== "function") return false;
  return window.matchMedia(query).matches;
}
