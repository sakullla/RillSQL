<script setup lang="ts">
import { computed } from "vue";
import type { ListboxOption } from "../ui/types";
import { tr as t } from "../i18n";
import Listbox from "../ui/Listbox.vue";
import { aboutAbsent, aboutFacts, aboutTagline, productTitle, PRODUCT_LICENSE, PRODUCT_VERSION } from "./product";
import { EDITOR_THEMES, localDataPathLabel, type AppLocale, type EditorTheme, type ShellSettings } from "./settings";
import { detectMac } from "./shortcuts";
import BrandMark from "./BrandMark.vue";
import Icon from "./Icon.vue";
import type { Density, ThemePreference } from "./theme";

const props = defineProps<{ settings: ShellSettings; dataDir: string | null }>();
const emit = defineEmits<{ change: [patch: Partial<ShellSettings>]; help: [] }>();

const mac = detectMac();
const linux = !mac && /Linux/i.test(globalThis.navigator?.userAgent ?? "");
const pathLabel = computed(() => props.dataDir || localDataPathLabel(mac, linux));

const themes: { id: ThemePreference; label: string; icon: "sun" | "moon" | "monitor" }[] = [
  { id: "light", label: "shell.theme.light", icon: "sun" },
  { id: "dark", label: "shell.theme.dark", icon: "moon" },
  { id: "system", label: "shell.theme.system", icon: "monitor" },
];

function scrollSection(id: string) { document.getElementById(id)?.scrollIntoView({ block: "start" }); }

function setTheme(theme: ThemePreference) {
  emit("change", { theme });
}
function setDensity(density: Density) {
  emit("change", { density });
}
const fontOptions = computed<ListboxOption[]>(() => [12, 13, 14, 15, 16].map((size) => ({ value: String(size), label: `${size} px` })));
const pageOptions = computed<ListboxOption[]>(() => [50, 100, 200, 500].map((size) => ({ value: String(size), label: t("common.rowsUnit", { n: size }) })));
const previewOptions = computed<ListboxOption[]>(() => [1, 4, 16].map((size) => ({ value: String(size), label: `${size} KB` })));
const objectOptions = computed<ListboxOption[]>(() => [100, 200, 500].map((size) => ({ value: String(size), label: t("common.itemsUnit", { n: size }) })));
const timeoutOptions = computed<ListboxOption[]>(() => [0, 10, 30, 60, 300].map((sec) => ({ value: String(sec), label: sec === 0 ? t("common.unlimited") : t("common.secondsUnit", { n: sec }) })));
const nullOptions = computed<ListboxOption[]>(() => [
  { value: "NULL", label: "NULL" },
  { value: "empty", label: t("common.emptyText") },
  { value: "N", label: "\\N" },
]);
const EDITOR_THEME_LABELS: Record<EditorTheme, string> = {
  auto: "shell.settings.editorThemeAuto",
  contrast: "shell.settings.editorThemeContrast",
  warm: "shell.settings.editorThemeWarm",
  cool: "shell.settings.editorThemeCool",
  mono: "shell.settings.editorThemeMono",
};
const editorThemeOptions = computed<ListboxOption[]>(() => EDITOR_THEMES.map((theme) => ({ value: theme, label: t(EDITOR_THEME_LABELS[theme]) })));
const localeOptions = computed<ListboxOption[]>(() => [
  { value: "zh-CN", label: t("common.localeZh") },
  { value: "en", label: t("common.localeEn") },
]);

function onNumber(key: "editorFont" | "pageSize" | "previewKB" | "objectPage" | "queryTimeoutSec", value: string) {
  const next = Number(value);
  if (!Number.isFinite(next)) return;
  emit("change", { [key]: next });
}
function onNullCopy(value: string) {
  if (value === "NULL" || value === "empty" || value === "N") emit("change", { nullCopy: value });
}
function onEditorTheme(value: string) {
  if (EDITOR_THEMES.includes(value as EditorTheme)) emit("change", { editorTheme: value as EditorTheme });
}
function onLocale(value: string) {
  if (value === "zh-CN" || value === "en") emit("change", { locale: value as AppLocale });
}
</script>

<template>
  <div class="page">
    <div class="page-inner narrow">
      <div class="page-head">
        <div>
          <h1>{{ t("shell.settings.title") }}</h1>
          <p>{{ t("shell.settings.subtitle") }}</p>
        </div>
      </div>
      <nav class="settings-nav" :aria-label="t('shell.settings.navLabel')"><button class="btn sm" @click="scrollSection('appearance')">{{ t("shell.settings.navAppearance") }}</button><button class="btn sm" @click="scrollSection('reading')">{{ t("shell.settings.navReading") }}</button><button class="btn sm" @click="scrollSection('behavior')">{{ t("shell.settings.navBehavior") }}</button><button class="btn sm" @click="scrollSection('local')">{{ t("shell.settings.navLocal") }}</button></nav>
      <div class="settings-body">
      <section id="appearance" class="set-sec">
        <h2><Icon name="sliders" />{{ t("shell.settings.navAppearance") }}</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.theme") }}</h4>
            <p>{{ t("shell.settings.themeHint") }}</p>
          </div>
          <div class="set-ctl">
            <div class="theme-cards">
              <label v-for="item in themes" :key="item.id" class="theme-card" :class="{ on: settings.theme === item.id }">
                <input type="radio" name="set-theme" :value="item.id" :checked="settings.theme === item.id" @change="setTheme(item.id)" />
                <span class="tc-prev" :class="`tc-${item.id}`"><i></i><i></i><i></i></span>
                <span><Icon :name="item.icon" />{{ t(item.label) }}</span>
              </label>
            </div>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.density") }}</h4>
            <p>{{ t("shell.settings.densityHint") }}</p>
          </div>
          <div class="set-ctl">
            <div class="seg-radio">
              <label :class="{ on: settings.density === 'compact' }">
                <input type="radio" name="set-density" value="compact" :checked="settings.density === 'compact'" @change="setDensity('compact')" />{{ t("shell.settings.densityCompact") }}
              </label>
              <label :class="{ on: settings.density === 'standard' }">
                <input type="radio" name="set-density" value="standard" :checked="settings.density === 'standard'" @change="setDensity('standard')" />{{ t("shell.settings.densityStandard") }}
              </label>
              <label :class="{ on: settings.density === 'relaxed' }">
                <input type="radio" name="set-density" value="relaxed" :checked="settings.density === 'relaxed'" @change="setDensity('relaxed')" />{{ t("shell.settings.densityRelaxed") }}
              </label>
            </div>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.language") }}</h4>
            <p>{{ t("shell.settings.languageHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="settings.locale" :options="localeOptions" :aria-label="t('shell.settings.language')" @update:model-value="onLocale" />
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.editorFont") }}</h4>
            <p>{{ t("shell.settings.editorFontHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="String(settings.editorFont)" :options="fontOptions" :aria-label="t('shell.settings.editorFont')" @update:model-value="onNumber('editorFont', $event)" />
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.editorTheme") }}</h4>
            <p>{{ t("shell.settings.editorThemeHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="settings.editorTheme" :options="editorThemeOptions" :aria-label="t('shell.settings.editorTheme')" @update:model-value="onEditorTheme" />
          </div>
        </div>
      </section>
      <section id="reading" class="set-sec">
        <h2><Icon name="table" />{{ t("shell.settings.navReading") }}</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.rowsPerPage") }}</h4>
            <p>{{ t("shell.settings.rowsPerPageHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="String(settings.pageSize)" :options="pageOptions" :aria-label="t('shell.settings.rowsPerPage')" @update:model-value="onNumber('pageSize', $event)" />
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.previewLimit") }}</h4>
            <p>{{ t("shell.settings.previewLimitHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="String(settings.previewKB)" :options="previewOptions" :aria-label="t('shell.settings.previewLimit')" @update:model-value="onNumber('previewKB', $event)" />
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.objectPage") }}</h4>
            <p>{{ t("shell.settings.objectPageHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="String(settings.objectPage)" :options="objectOptions" :aria-label="t('shell.settings.objectPage')" @update:model-value="onNumber('objectPage', $event)" />
          </div>
        </div>
      </section>
      <section id="behavior" class="set-sec">
        <h2><Icon name="play" />{{ t("shell.settings.navBehavior") }}</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.timeout") }}</h4>
            <p>{{ t("shell.settings.timeoutHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="String(settings.queryTimeoutSec)" :options="timeoutOptions" :aria-label="t('shell.settings.timeout')" @update:model-value="onNumber('queryTimeoutSec', $event)" />
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.nullCopy") }}</h4>
            <p>{{ t("shell.settings.nullCopyHint") }}</p>
          </div>
          <div class="set-ctl">
            <Listbox :model-value="settings.nullCopy" :options="nullOptions" :aria-label="t('shell.settings.nullCopy')" @update:model-value="onNullCopy" />
          </div>
        </div>
      </section>
      <section id="local" class="set-sec">
        <h2><Icon name="lock" />{{ t("shell.settings.navLocal") }}</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.localTitle") }}</h4>
            <p>{{ t("shell.settings.localHint") }}</p>
          </div>
          <div class="set-ctl"><code class="path">{{ pathLabel }}</code></div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>{{ t("shell.settings.shortcutsTitle") }}</h4>
            <p>{{ t("shell.settings.shortcutsHint") }}</p>
          </div>
          <div class="set-ctl">
            <button class="btn" type="button" @click="emit('help')"><Icon name="keyboard" />{{ t("shell.settings.viewShortcuts") }}</button>
          </div>
        </div>
      </section>
      <section class="set-sec about">
        <div class="about-card">
          <BrandMark large />
          <div>
            <h3>{{ productTitle() }}</h3>
            <p>{{ t("shell.settings.version", { version: PRODUCT_VERSION, license: PRODUCT_LICENSE }) }}</p>
            <p class="muted sm">{{ aboutTagline() }}</p>
            <p class="about-absent">{{ aboutAbsent() }}</p>
            <ul class="about-facts">
              <li v-for="fact in aboutFacts()" :key="fact"><Icon name="check" />{{ fact }}</li>
            </ul>
          </div>
        </div>
      </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 左侧分区导航 + 右侧设置列：长页面不用来回滚 */
.page-inner.narrow { display: grid; grid-template-columns: 160px minmax(0, 1fr); gap: 0 var(--sp-7); align-items: start; }
.page-inner.narrow > .page-head { grid-column: 1 / -1; }
.settings-nav { position: sticky; top: 0; display: flex; flex-direction: column; gap: 2px; margin: 0; }
.settings-nav .btn { justify-content: flex-start; width: 100%; height: var(--control-h); background: transparent; border-color: transparent; box-shadow: none; color: var(--text-2); font-size: var(--fs-sm); }
.settings-nav .btn:hover:not(:disabled) { background: var(--hover); color: var(--text); }
.settings-body { min-width: 0; }
section.set-sec { margin-bottom: var(--sp-6); }
section.set-sec :deep(.set-text h4) { font-weight: 550; }
section.set-sec > h2 {
  display: flex;
  align-items: center;
  gap: var(--sp-2);
  margin: 0;
  padding: 0 0 var(--sp-2);
  font-size: var(--fs-md);
  color: var(--text);
  border-bottom: 1px solid var(--line-2);
}
section.set-sec > h2 .i { width: 14px; height: 14px; flex-shrink: 0; }
section.set-sec:not(.about) .set-row:last-child { border-bottom: 0; padding-bottom: var(--sp-3); }
@media (max-width: 800px) {
  .page-inner.narrow { display: block; }
  .settings-nav { position: static; flex-direction: row; flex-wrap: wrap; margin-bottom: var(--sp-4); }
  .settings-nav .btn { width: auto; }
}
@media (max-width: 720px) {
  section.set-sec:not(.about) .set-row { flex-wrap: wrap; align-items: flex-start; }
}
</style>
