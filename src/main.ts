import { createApp } from "vue";
import AppShell from "./shell/AppShell.vue";
import { loadSettings } from "./shell/settings";
import { applyDocumentTheme, prefersDarkScheme } from "./shell/theme";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/shell.css";
import "./styles/workbench.css";

const stored = loadSettings();
applyDocumentTheme(stored.theme, stored.density, prefersDarkScheme(), stored.editorFont);

createApp(AppShell).mount("#app");
