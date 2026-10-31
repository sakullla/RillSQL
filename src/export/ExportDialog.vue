<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { tr as t } from "../i18n";
import type { ListboxOption } from "../ui/types";
import Listbox from "../ui/Listbox.vue";
import { renderCsv, type CsvCell, type CsvDelimiter, type CsvOptions, type NullToken } from "./csv";
import { renderInsert, type InsertTarget } from "./insert";
import { renderJson } from "./json";
import { renderMarkdown } from "./markdown";
import { planExport, type ExportOrigin, type ExportScope } from "./plan";
import { chooseExportPath } from "./native";
import type { ExportFormat } from "./write";
import { renderXlsxPreview } from "./xlsx";

const props = defineProps<{
  open: boolean;
  origin: ExportOrigin;
  statement?: string;
  engine?: string;
  truncated: boolean;
  fileStem: string;
  loadedCount: number;
  pending: number;
  columns: string[];
  previewRows: Record<string, CsvCell>[];
  insertTarget?: InsertTarget | null;
  insertReason?: string;
}>();

const emit = defineEmits<{
  close: [];
  start: [payload: { planNotice: string; scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions; format: ExportFormat }];
}>();

const delimiterOptions: ListboxOption[] = [
  { value: ",", label: t("export.dialog.comma") },
  { value: ";", label: t("export.dialog.semicolon") },
  { value: "\t", label: t("export.dialog.tab") },
];
const nullOptions: ListboxOption[] = [
  { value: "", label: t("export.dialog.noQuote") },
  { value: "\\N", label: "\\N" },
  { value: "NULL", label: t("export.dialog.nullToken") },
];
const encodingOptions: ListboxOption[] = [
  { value: "utf8", label: "UTF-8" },
  { value: "bom", label: t("export.dialog.bom") },
];

const scope = ref<ExportScope>("loaded");
const format = ref<ExportFormat>("csv");
const delimiter = ref<CsvDelimiter>(",");
const nullToken = ref<NullToken>("");
const bom = ref(true);
const header = ref(true);
const path = ref("");
const acknowledged = ref(false);
const choosing = ref(false);
const error = ref("");

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    scope.value = "loaded";
    format.value = "csv";
    acknowledged.value = false;
    error.value = "";
    const stamp = new Date().toISOString().slice(0, 10);
    path.value = `${props.fileStem}-${stamp}.csv`;
  },
  { immediate: true },
);

function formatExtension(next: ExportFormat): string {
  if (next === "json") return "json";
  if (next === "insert") return "sql";
  if (next === "markdown") return "md";
  if (next === "xlsx") return "xlsx";
  return "csv";
}

function exportTitle(next: ExportFormat): string {
  if (next === "json") return t("export.dialog.titleJson");
  if (next === "insert") return t("export.dialog.titleInsert");
  if (next === "markdown") return t("export.dialog.titleMarkdown");
  if (next === "xlsx") return t("export.dialog.titleXlsx");
  return t("export.dialog.titleCsv");
}

watch(format, (next, previous) => {
  if (!props.open || next === previous) return;
  const extension = formatExtension(next);
  path.value = path.value.replace(/\.(csv|json|sql|md|xlsx)$/i, `.${extension}`);
  if (!/\.(csv|json|sql|md|xlsx)$/i.test(path.value)) path.value = `${path.value}.${extension}`;
});

const csv = computed<CsvOptions>(() => ({ delimiter: delimiter.value, nullToken: nullToken.value, bom: bom.value, header: header.value }));
const plan = computed(() =>
  planExport({
    origin: props.origin,
    scope: scope.value,
    statement: props.statement,
    truncated: props.truncated,
    fileName: path.value.trim() || `${props.fileStem}.csv`,
    engine: props.engine,
  }),
);
const insertBlocked = computed(() => format.value === "insert" && !props.insertTarget?.name);
const title = computed(() => exportTitle(format.value));
const preview = computed(() => {
  const rows = props.previewRows.slice(0, 4);
  if (format.value === "json") return renderJson(props.columns, rows);
  if (format.value === "markdown") return renderMarkdown(props.columns, rows, { nullToken: nullToken.value });
  if (format.value === "xlsx") return renderXlsxPreview(props.columns, rows);
  if (format.value === "insert" && props.insertTarget?.name) return renderInsert(props.insertTarget, props.columns, rows);
  if (format.value === "insert") return "";
  return renderCsv(props.columns, rows, { ...csv.value, bom: false });
});
const canStart = computed(() => plan.value.allowed && !insertBlocked.value && (!plan.value.reexecutes || acknowledged.value) && path.value.trim().length > 0);

function onDelimiter(value: string) {
  if (value === "," || value === ";" || value === "\t") delimiter.value = value;
}

function onNullToken(value: string) {
  if (value === "" || value === "\\N" || value === "NULL") nullToken.value = value;
}

function onEncoding(value: string) {
  if (value === "utf8" || value === "bom") bom.value = value === "bom";
}

async function start() {
  if (!canStart.value || choosing.value) return;
  choosing.value = true;
  error.value = "";
  const payload = {
    planNotice: plan.value.notice,
    scope: scope.value,
    acknowledged: acknowledged.value,
    path: plan.value.fileName,
    csv: { ...csv.value },
    format: format.value,
  };
  try {
    const selected = await chooseExportPath(payload.path, format.value);
    if (!selected || !props.open) return;
    path.value = selected;
    emit("start", { ...payload, path: selected });
  } catch (cause) {
    error.value = t("export.common.dialogFailed", { error: cause instanceof Error ? cause.message : String(cause) });
  } finally {
    choosing.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="ex-overlay" role="presentation">
    <div class="ex-sheet" role="dialog" aria-modal="true" :aria-label="title">
      <header class="ex-head">
        <div class="ex-titles">
          <h2>{{ title }}</h2>
          <p>{{ t("export.dialog.notBackup") }}</p>
        </div>
        <button class="icon-btn" type="button" :aria-label="t('common.close')" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>

      <div class="ex-body">
        <p v-if="error" class="ex-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>{{ error }}</p>
        <fieldset class="ex-options" :disabled="choosing">
        <p v-if="pending" class="ex-note warn">{{ t("export.dialog.pendingNote", { n: pending }) }}</p>

        <fieldset class="ex-scope">
          <legend>{{ t("export.dialog.formatLegend") }}</legend>
          <label class="ex-choice" :class="{ on: format === 'csv' }">
            <input v-model="format" type="radio" name="ex-format" value="csv" />
            <span><b>{{ t("export.dialog.csvLabel") }}</b><small>{{ t("export.dialog.csvDesc") }}</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'json' }">
            <input v-model="format" type="radio" name="ex-format" value="json" />
            <span><b>{{ t("export.dialog.jsonLabel") }}</b><small>{{ t("export.dialog.jsonDesc") }}</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'xlsx' }">
            <input v-model="format" type="radio" name="ex-format" value="xlsx" />
            <span><b>{{ t("export.dialog.xlsxLabel") }}</b><small>{{ t("export.dialog.xlsxDesc") }}</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'markdown' }">
            <input v-model="format" type="radio" name="ex-format" value="markdown" />
            <span><b>{{ t("export.dialog.markdownLabel") }}</b><small>{{ t("export.dialog.markdownDesc") }}</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'insert' }">
            <input v-model="format" type="radio" name="ex-format" value="insert" />
            <span><b>{{ t("export.dialog.insertLabel") }}</b><small>{{ t("export.dialog.insertDesc") }}</small></span>
          </label>
        </fieldset>
        <p v-if="insertBlocked" class="ex-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>{{ insertReason || t("export.dialog.insertBlocked") }}</p>

        <fieldset class="ex-scope">
          <legend>{{ t("export.dialog.rowsLegend") }}</legend>
          <label class="ex-choice" :class="{ on: scope === 'loaded' }">
            <input v-model="scope" type="radio" name="ex-scope" value="loaded" />
            <span>
              <b>{{ t("export.dialog.loadedOnly", { n: loadedCount }) }}</b>
              <small>{{ truncated ? t("export.dialog.loadedTruncatedNote") : t("export.dialog.loadedCompleteNote") }}</small>
            </span>
          </label>
          <label class="ex-choice" :class="{ on: scope === 'full' }">
            <input v-model="scope" type="radio" name="ex-scope" value="full" />
            <span>
              <b>{{ origin === "query" ? t("export.dialog.reexecuteQuery") : t("export.dialog.filterAll") }}</b>
              <small>{{ origin === "query" ? t("export.dialog.reexecuteQueryNote") : t("export.dialog.filterAllNote") }}</small>
            </span>
          </label>
        </fieldset>

        <p v-if="plan.notice" class="ex-note" data-export-notice>{{ plan.notice }}</p>
        <p v-if="plan.rejectReason" class="ex-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>{{ plan.rejectReason }}</p>
        <label v-if="plan.reexecutes" class="ex-ack">
          <input v-model="acknowledged" type="checkbox" />
          {{ t("export.dialog.acknowledge") }}
        </label>

        <label class="ex-path">
          {{ t("export.common.saveTo") }}
          <input v-model="path" :aria-label="t('export.common.exportPath')" spellcheck="false" />
        </label>

        <div class="ex-preview-wrap">
          <p v-if="format === 'csv'" class="ex-note">{{ t("export.dialog.csvPreviewNote", { token: nullToken === "" ? t("export.dialog.csvPreviewEmpty") : nullToken }) }}</p>
          <p v-else-if="format === 'xlsx'" class="ex-note">{{ t("export.dialog.xlsxPreviewNote") }}</p>
          <p v-else class="ex-note">{{ t("export.dialog.otherPreviewNote") }}</p>
          <pre class="ex-preview">{{ preview }}</pre>
        </div>

        <details v-if="format === 'csv'" class="disclosure">
          <summary>{{ t("export.dialog.advanced") }} <span class="muted">{{ t("export.dialog.advancedHint") }}</span></summary>
          <div class="disclosure-body ex-advanced">
            <div class="ex-field">
              <label for="export-delimiter">{{ t("export.dialog.delimiter") }}</label>
              <Listbox id="export-delimiter" :model-value="delimiter" :options="delimiterOptions" :aria-label="t('export.dialog.delimiter')" :disabled="choosing" @update:model-value="onDelimiter" />
            </div>
            <div class="ex-field">
              <label for="export-null">{{ t("export.dialog.nullWrite") }}</label>
              <Listbox id="export-null" :model-value="nullToken" :options="nullOptions" :aria-label="t('export.dialog.nullWriteAria')" :disabled="choosing" @update:model-value="onNullToken" />
            </div>
            <div class="ex-field">
              <label for="export-encoding">{{ t("export.dialog.encoding") }}</label>
              <Listbox id="export-encoding" :model-value="bom ? 'bom' : 'utf8'" :options="encodingOptions" :aria-label="t('export.dialog.encoding')" :disabled="choosing" @update:model-value="onEncoding" />
            </div>
            <label class="ex-header-row"><input v-model="header" type="checkbox" /> {{ t("export.dialog.headerRow") }}</label>
          </div>
        </details>
        </fieldset>
      </div>

      <footer class="ex-foot">
        <span class="muted sm">{{ plan.label }}{{ plan.partial ? t("export.dialog.partialSuffix") : "" }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">{{ t("common.cancel") }}</button>
        <button class="btn primary" type="button" :disabled="!canStart || choosing" @click="start">{{ choosing ? t("export.common.chooseSaveLocation") : t("export.dialog.exportButton") }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
/* 对话框：与 base.css 的 dialog.modal 同一套 overlay 表面体系。 */
.ex-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.ex-sheet { width: min(720px, 100%); max-height: min(88vh, 880px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.ex-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.ex-titles { flex: 1; min-width: 0; }
.ex-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.ex-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); }
.ex-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.ex-options { margin: 0; padding: 0; border: 0; min-width: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-advanced :deep(.listbox) { width: 100%; }
.ex-advanced :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.ex-note { margin: 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.ex-note.warn { padding: var(--sp-2) var(--sp-3); border: 1px solid var(--warn-line); border-radius: var(--r); background: var(--warn-soft); color: var(--warn); }
.num { font-family: var(--mono); font-variant-numeric: tabular-nums; }
/* 错误卡：与全局 err 模式一致（软底卡 + 同色 3px 左缘 + 语义图标） */
.ex-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); line-height: var(--lh); }
.ex-alert > .i { width: 14px; height: 14px; flex-shrink: 0; margin-top: 2px; }

/* 格式 / 范围选择：实体选择卡（隐藏原生单选，选中以强调描边 + 指示点表达） */
.ex-scope { margin: 0; padding: 0; border: 0; display: flex; flex-direction: column; gap: var(--sp-2); min-width: 0; }
.ex-scope legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-sm); font-weight: 600; color: var(--text-3); }
.ex-choice { position: relative; display: flex; gap: var(--sp-2); align-items: flex-start; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface-panel); cursor: pointer; transition: border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease); }
.ex-choice:hover { border-color: var(--line-2); background: var(--hover); }
.ex-choice.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent-soft) 45%, var(--surface-panel)); box-shadow: 0 0 0 1px var(--accent); }
.ex-choice.on::after { content: ""; position: absolute; top: var(--sp-2); right: var(--sp-2); width: 6px; height: 6px; border-radius: var(--r-full); background: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 25%, transparent); }
.ex-choice:focus-within { outline: 2px solid var(--accent); outline-offset: 1px; }
.ex-choice input { position: absolute; opacity: 0; inset: 0; margin: 0; cursor: pointer; }
.ex-choice b { font-size: var(--fs-sm); font-weight: 600; }
.ex-choice small { display: block; color: var(--text-2); font-size: var(--fs-xs); line-height: var(--lh); margin-top: 2px; }
.ex-choice .num { color: var(--text); }

/* 重执行确认：警告软底卡 */
.ex-ack { display: flex; gap: var(--sp-2); align-items: flex-start; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--warn-line); border-radius: var(--r); background: var(--warn-soft); font-size: var(--fs-sm); color: var(--text); line-height: var(--lh); cursor: pointer; }
.ex-ack input { margin-top: 2px; }
.ex-path { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.ex-path input { width: 100%; font-family: var(--mono); font-size: var(--fs-xs); }

/* 预览：下沉井 + 内描边 */
.ex-preview-wrap { display: flex; flex-direction: column; gap: var(--sp-1); }
.ex-preview { margin: 0; max-height: 140px; overflow: auto; padding: var(--sp-2) var(--sp-3); border-radius: var(--r); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); font-family: var(--mono); font-size: var(--fs-xs); line-height: var(--lh); white-space: pre-wrap; word-break: break-all; }
.ex-advanced { display: flex; gap: var(--sp-3); flex-wrap: wrap; align-items: flex-end; }
.ex-field { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); }
.ex-advanced label.ex-header-row { flex-direction: row; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); color: var(--text); }
</style>
