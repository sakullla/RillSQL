import { createApp } from "vue";
import { i18n } from "./i18n";
import AppShell from "./shell/AppShell.vue";
import { loadSettings } from "./shell/settings";
import { applyDocumentTheme, prefersDarkScheme } from "./shell/theme";
import "./styles/tokens.css";
import "./styles/fonts.css";
import "./styles/base.css";
import "./styles/shell.css";
import "./styles/workbench.css";

const stored = loadSettings();
applyDocumentTheme(stored.theme, stored.density, prefersDarkScheme(), stored.editorFont);

createApp(AppShell).use(i18n).mount("#app");
