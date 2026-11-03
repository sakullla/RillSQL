<script setup lang="ts">
import { tr as t } from "../i18n";
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch, type ComponentPublicInstance } from "vue";
import type { ListboxOption } from "../ui/types";
import Listbox from "../ui/Listbox.vue";
import { catalogConnections } from "../catalog/store";
import { gridCommit, gridDescribe } from "../grid/api";
import type { TableTarget } from "../grid/target";
import type { TableSchema } from "../grid/types";
import { useRowWindow } from "../grid/virtual";
import { showCell } from "../grid/display";
import type { CsvDelimiter } from "./csv";
import {
  importChanges,
  importResultText,
  mappedPreview,
  parseImportCsv,
  parseImportJson,
  requiredGaps,
  suggestMapping,
  type ImportSeed,
  type ParsedImport,
} from "./import";

const props = defineProps<{
  open: boolean;
  target: TableTarget;
  schema: TableSchema;
  tables?: { schema: string; table: string }[];
  seed?: ImportSeed | null;
}>();

const emit = defineEmits<{
  close: [];
  done: [message: string];
}>();

const format = ref<"csv" | "json" | "xlsx">("csv");
const delimiter = ref<CsvDelimiter>(",");
const lastText = ref("");
const lastBytes = ref<Uint8Array | null>(null);
const tableKey = ref("");
const localSchema = ref<TableSchema | null>(null);
const localTarget = ref<TableTarget | null>(null);
const parsed = shallowRef<ParsedImport | null>(null);
let parseVersion = 0;
let fileVersion = 0;
const mapping = ref<Record<string, string>>({});
const parseError = ref("");
const resultText = ref("");
const writing = ref(false);
const describing = ref(false);
/** 拖放接进来时展示来源文件名，提示内容已读取、仍可另选文件。 */
const droppedName = ref("");
/** 应用内文件按钮（R4）：隐藏原生 input，由按钮触发，并展示已选文件名。 */
const fileInput = ref<HTMLInputElement | null>(null);
const chosenName = ref("");
/** 打开时记住触发元素，关闭后若仍在文档中则还焦。 */
const returnFocus = ref<HTMLElement | null>(null);

function rememberFocus() {
  const active = document.activeElement;
  returnFocus.value = active instanceof HTMLElement ? active : null;
}

function closeDialog() {
  const target = returnFocus.value;
  emit("close");
  void nextTick(() => {
    if (target?.isConnected) target.focus();
  });
}

function onDialogEscape(event: KeyboardEvent) {
  if (event.defaultPrevented) return;
  event.preventDefault();
  closeDialog();
}

const choices = computed(() => {
  const list = [...(props.tables ?? [])];
  if (!list.some((item) => item.schema === props.target.schema && item.table === props.target.table)) {
    list.unshift({ schema: props.target.schema, table: props.target.table });
  }
  return list;
});
const tableOptions = computed<ListboxOption[]>(() =>
  choices.value.map((item) => ({
    value: keyOf(item.schema, item.table),
    label: item.schema ? `${item.schema}.${item.table}` : item.table,
  })),
);
const columnOptions = computed<ListboxOption[]>(() => [
  { value: "", label: t("export.importDialog.skip") },
  ...(parsed.value?.columns ?? []).map((name) => ({ value: name, label: name })),
]);
const delimiterOptions: ListboxOption[] = [
  { value: ",", label: t("export.dialog.comma") },
  { value: ";", label: t("export.dialog.semicolon") },
  { value: "\t", label: t("export.dialog.tab") },
];

function keyOf(schema: string, table: string): string {
  return `${schema}\u0000${table}`;
}

/** 打开时的复位；拖放 seed 在复位后直接走 applyText，与手动选文件同一条解析路径。 */
function resetForOpen(): void {
  parseVersion += 1;
  fileVersion += 1;
  lastText.value = "";
  lastBytes.value = null;
  parsed.value = null;
  mapping.value = {};
  parseError.value = "";
  resultText.value = "";
  format.value = "csv";
  delimiter.value = ",";
  writing.value = false;
  localSchema.value = props.schema;
  localTarget.value = props.target;
  tableKey.value = keyOf(props.target.schema, props.target.table);
  droppedName.value = props.seed?.fileName ?? "";
  chosenName.value = "";
  if (fileInput.value) fileInput.value.value = "";
  if (props.seed) {
    format.value = props.seed.format;
    delimiter.value = props.seed.delimiter ?? ",";
    lastText.value = props.seed.text;
    applyText(props.seed.text);
  }
}

watch(() => props.open, (open) => {
  if (open) {
    rememberFocus();
    resetForOpen();
  } else releaseImportData();
});

function releaseImportData() {
  parseVersion += 1;
  fileVersion += 1;
  parsed.value = null;
  lastText.value = "";
  lastBytes.value = null;
}
onBeforeUnmount(releaseImportData);

watch(format, (next) => {
  parseVersion += 1;
  parsed.value = null;
  mapping.value = {};
  parseError.value = "";
  resultText.value = "";
  if (next === "xlsx") {
    if (lastBytes.value) void applyBytes(lastBytes.value);
    return;
  }
  if (lastText.value) applyText(lastText.value);
});

const activeSchema = computed(() => localSchema.value ?? props.schema);
const boundConnection = computed(() => catalogConnections().find((item) => item.id === props.target.connectionId) ?? null);
const connectionName = computed(() => boundConnection.value?.name.trim() || props.target.connectionId);
const environmentLabel = computed(() => boundConnection.value?.envLabel.trim() ?? "");
const environmentTone = computed(() => boundConnection.value?.envTone ?? "none");
const objectName = computed(() => {
  const current = localTarget.value ?? props.target;
  return current.schema ? `${current.schema}.${current.table}` : current.table;
});
const gaps = computed(() => requiredGaps(activeSchema.value.columns, mapping.value));
const previewRows = computed(() => (parsed.value ? mappedPreview(parsed.value.rows, mapping.value, activeSchema.value.columns) : []));
const { viewport, range, onScroll } = useRowWindow(computed(() => previewRows.value.length));
const visibleRows = computed(() => previewRows.value.slice(range.value.start, range.value.end));
const canWrite = computed(() => !!parsed.value && parsed.value.rows.length > 0 && gaps.value.length === 0 && !writing.value && !describing.value && !parseError.value && activeSchema.value.kind === "table");

function setViewport(el: Element | ComponentPublicInstance | null) {
  viewport.value = el instanceof HTMLElement ? el : null;
}

function onTable(value: string) {
  if (writing.value || value === tableKey.value) return;
  tableKey.value = value;
  void chooseTable();
}

function onColumn(name: string, value: string) {
  if (writing.value) return;
  mapping.value[name] = value;
}

async function chooseTable() {
  const [schemaName, table] = tableKey.value.split("\u0000");
  if (!table || !localTarget.value) return;
  if (schemaName === props.target.schema && table === props.target.table) {
    localSchema.value = props.schema;
    localTarget.value = props.target;
    if (parsed.value) mapping.value = suggestMapping(props.schema.columns, parsed.value.columns);
    return;
  }
  describing.value = true;
  parseError.value = "";
  try {
    const nextTarget = { ...props.target, schema: schemaName ?? "", table };
    const described = await gridDescribe(nextTarget);
    localTarget.value = nextTarget;
    localSchema.value = described;
    if (described.kind !== "table") parseError.value = t("export.common.onlyTableImport");
    if (parsed.value) mapping.value = suggestMapping(described.columns, parsed.value.columns);
  } catch (error) {
    parseError.value = error instanceof Error ? error.message : String(error);
  } finally {
    describing.value = false;
  }
}

async function applyBytes(bytes: Uint8Array) {
  const version = ++parseVersion;
  let parseImportXlsx: typeof import("./xlsx").parseImportXlsx;
  try {
    ({ parseImportXlsx } = await import("./xlsx"));
  } catch (error) {
    if (version === parseVersion) parseError.value = error instanceof Error ? error.message : String(error);
    return;
  }
  if (version !== parseVersion || !props.open) return;
  const outcome = parseImportXlsx(bytes);
  if (!outcome.ok) {
    parseError.value = outcome.reason;
    parsed.value = null;
    mapping.value = {};
    return;
  }
  parseError.value = "";
  parsed.value = outcome.data;
  mapping.value = suggestMapping(activeSchema.value.columns, outcome.data.columns);
}

function applyText(text: string) {
  parseVersion += 1;
  const outcome = format.value === "json" ? parseImportJson(text) : parseImportCsv(text, delimiter.value);
  if (!outcome.ok) {
    parseError.value = outcome.reason;
    parsed.value = null;
    mapping.value = {};
    return;
  }
  parseError.value = "";
  parsed.value = outcome.data;
  mapping.value = suggestMapping(activeSchema.value.columns, outcome.data.columns);
}

function pickFile() {
  if (writing.value) return;
  fileInput.value?.click();
}

async function onFile(event: Event) {
  const version = ++fileVersion;
  parseVersion += 1;
  const file = (event.target as HTMLInputElement).files?.[0];
  resultText.value = "";
  parsed.value = null;
  if (!file) return;
  chosenName.value = file.name;
  droppedName.value = "";
  if (format.value === "xlsx" || file.name.toLowerCase().endsWith(".xlsx")) {
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (version !== fileVersion || !props.open) return;
    lastBytes.value = bytes;
    lastText.value = "";
    if (format.value !== "xlsx") format.value = "xlsx";
    else await applyBytes(bytes);
    return;
  }
  const text = await file.text();
  if (version !== fileVersion || !props.open) return;
  lastText.value = text;
  lastBytes.value = null;
  applyText(text);
}

function onDelimiter(value: string) {
  if (value !== "," && value !== ";" && value !== "\t") return;
  delimiter.value = value;
  if (!lastText.value || format.value !== "csv") return;
  parsed.value = null;
  applyText(lastText.value);
}

async function confirm() {
  if (!canWrite.value || !parsed.value || !localTarget.value) return;
  writing.value = true;
  try {
    const changes = importChanges(parsed.value.rows, mapping.value, activeSchema.value.columns);
    const report = await gridCommit(localTarget.value, changes);
    const summary = importResultText(report, parsed.value.rows.length);
    resultText.value = summary.text;
    emit("done", summary.text);
  } catch (error) {
    resultText.value = t("export.importDialog.incomplete", { error: error instanceof Error ? error.message : String(error) });
  } finally {
    writing.value = false;
  }
}

// 初次挂载即按 open 状态复位（原先由 immediate watch 承担，移到声明之后以便 seed 能用同一套 computed）。
if (props.open) rememberFocus();
resetForOpen();
</script>

<template>
  <div v-if="open" class="ex-overlay" role="presentation">
    <div class="ex-sheet" role="dialog" aria-modal="true" :aria-label="t('export.importDialog.dialogLabel')" @keydown.escape="onDialogEscape">
      <header class="ex-head">
        <div class="ex-titles">
          <h2>{{ t("export.importDialog.title") }}</h2>
          <p>{{ t("export.importDialog.subtitle") }}</p>
        </div>
        <button class="icon-btn" type="button" :aria-label="t('common.close')" @click="closeDialog">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>
      <div class="ex-body">
        <p class="ex-target" data-import-target>{{ t("export.importDialog.target") }}<b>{{ connectionName }}</b><span class="ex-target-sep" aria-hidden="true">/</span><b class="mono">{{ objectName }}</b> <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p>
        <p v-if="parseError" class="ex-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>{{ parseError }}</p>
        <p v-if="resultText" class="ex-note" role="status" data-import-result>{{ resultText }}</p>
        <div class="ex-path">
          <label for="import-table">{{ t("export.importDialog.importTo") }}</label>
          <Listbox id="import-table" :model-value="tableKey" :options="tableOptions" :aria-label="t('export.importDialog.tableAria')" :disabled="writing" @update:model-value="onTable" />
        </div>
        <fieldset class="ex-scope">
          <legend>{{ t("export.importDialog.formatLegend") }}</legend>
          <label class="ex-choice" :class="{ on: format === 'csv' }"><input v-model="format" type="radio" name="im-format" value="csv" :disabled="writing" /><span><b>{{ t("export.importDialog.csvLabel") }}</b><small>{{ t("export.importDialog.csvHint") }}</small></span></label>
          <label class="ex-choice" :class="{ on: format === 'json' }"><input v-model="format" type="radio" name="im-format" value="json" :disabled="writing" /><span><b>{{ t("export.importDialog.jsonLabel") }}</b><small>{{ t("export.importDialog.jsonHint") }}</small></span></label>
          <label class="ex-choice" :class="{ on: format === 'xlsx' }"><input v-model="format" type="radio" name="im-format" value="xlsx" :disabled="writing" /><span><b>{{ t("export.importDialog.xlsxLabel") }}</b><small>{{ t("export.importDialog.xlsxHint") }}</small></span></label>
        </fieldset>
        <div v-if="format === 'csv'" class="ex-path">
          <label for="import-delimiter">{{ t("export.dialog.delimiter") }}</label>
          <Listbox id="import-delimiter" :model-value="delimiter" :options="delimiterOptions" :aria-label="t('export.importDialog.delimiterAria')" :disabled="writing" @update:model-value="onDelimiter" />
        </div>
        <div class="ex-path ex-file">
          <span class="ex-file-label">{{ t("export.importDialog.chooseFile") }}</span>
          <div class="ex-file-row">
            <button class="btn sm" type="button" data-action="pick-import-file" :disabled="writing" @click="pickFile">
              <svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M13 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM13 3v5h5" /></svg>
              {{ t("export.importDialog.pickFileButton") }}
            </button>
            <span class="ex-file-name" :class="{ none: !chosenName }" :title="chosenName || undefined">{{ chosenName ? t("export.importDialog.fileChosen", { name: chosenName }) : t("export.importDialog.noFileChosen") }}</span>
          </div>
          <input ref="fileInput" class="ex-file-input" type="file" :aria-label="t('export.importDialog.fileAria')" accept=".csv,.tsv,.json,.txt,.xlsx,text/csv,application/json,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" :disabled="writing" @change="onFile" />
        </div>
        <p v-if="droppedName" class="ex-note" data-dropped-file>{{ t("export.importDialog.droppedRead", { name: droppedName }) }}</p>
        <p v-if="parsed && gaps.length" class="ex-alert" role="alert"><svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18M15 9l-6 6M9 9l6 6" /></svg>{{ t("export.importDialog.gapsBlocked", { fields: gaps.join(t("common.listJoin")) }) }}</p>
        <table v-if="parsed" class="map-table">
          <thead><tr><th>{{ t("export.importDialog.columnTableField") }}</th><th>{{ t("export.importDialog.columnFileField") }}</th></tr></thead>
          <tbody>
            <tr v-for="column in activeSchema.columns" :key="column.name">
              <td>{{ column.name }}<small v-if="!column.nullable && !column.auto" class="req">{{ t("export.importDialog.required") }}</small></td>
              <td>
                <Listbox :model-value="mapping[column.name] ?? ''" :options="columnOptions" :aria-label="t('export.importDialog.columnMapAria', { column: column.name })" :disabled="writing" @update:model-value="onColumn(column.name, $event)" />
              </td>
            </tr>
          </tbody>
        </table>
        <p v-if="parsed" class="ex-note">{{ t("export.importDialog.willWrite", { n: parsed.rows.length }) }}</p>
        <div v-if="parsed" :ref="setViewport" class="preview-scroll" @scroll.passive="onScroll">
          <table class="map-table">
            <thead><tr><th v-for="column in activeSchema.columns" :key="column.name">{{ column.name }}</th></tr></thead>
            <tbody>
              <tr v-if="range.top"><td :colspan="activeSchema.columns.length" :style="{ height: `${range.top}px` }"></td></tr>
              <tr v-for="(row, index) in visibleRows" :key="range.start + index">
                <td v-for="column in activeSchema.columns" :key="column.name">{{ showCell(row[column.name] ?? null).label }}</td>
              </tr>
              <tr v-if="range.bottom"><td :colspan="activeSchema.columns.length" :style="{ height: `${range.bottom}px` }"></td></tr>
            </tbody>
          </table>
        </div>
      </div>
      <footer class="ex-foot">
        <span class="muted sm">{{ describing ? t("export.importDialog.describing") : writing ? t("export.importDialog.writing") : t("export.importDialog.confirmFirst") }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" data-action="import-cancel" :disabled="writing" @click="closeDialog">{{ t("common.cancel") }}</button>
        <button class="btn primary" type="button" data-action="import-confirm" :disabled="!canWrite" @click="confirm">{{ t("export.importDialog.importButton") }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
/* 对话框：与 base.css 的 dialog.modal 同一套 overlay 表面体系。 */
.ex-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.ex-sheet { width: min(720px, 100%); max-height: min(88vh, 880px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.ex-head { flex-shrink: 0; display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.ex-titles { flex: 1; min-width: 0; }
.ex-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.ex-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); }
.ex-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-foot { flex-shrink: 0; display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.ex-note { margin: 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
/* 错误卡：与全局 err 模式一致（软底卡 + 同色 3px 左缘 + 语义图标） */
.ex-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); line-height: var(--lh); }
.ex-alert > .i { width: 14px; height: 14px; flex-shrink: 0; margin-top: 2px; }
.ex-path :deep(.listbox),
.map-table :deep(.listbox) { width: 100%; }
.ex-path :deep(.listbox-trigger),
.map-table :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }

/* 写入目标条：panel 实体卡 + mono 对象名 */
.ex-target { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; margin: 0; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--stroke); border-radius: var(--r-md); background: var(--surface-panel); box-shadow: var(--shadow-sm); font-size: var(--fs-sm); color: var(--text-2); }
.ex-target b { font-weight: 600; color: var(--text); }
.ex-target .mono { font-size: var(--fs-xs); }
.ex-target-sep { color: var(--text-3); }

/* 格式选择：实体选择卡（隐藏原生单选，选中以强调描边 + 指示点表达） */
.ex-scope { margin: 0; padding: 0; border: 0; display: flex; flex-direction: column; gap: var(--sp-2); min-width: 0; }
.ex-scope legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-sm); font-weight: 600; color: var(--text-3); }
.ex-choice { position: relative; display: flex; gap: var(--sp-2); align-items: flex-start; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r-md); background: var(--surface-panel); cursor: pointer; transition: border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease); }
.ex-choice:hover { border-color: var(--line-2); background: var(--hover); }
.ex-choice.on { border-color: var(--accent); background: color-mix(in srgb, var(--accent-soft) 45%, var(--surface-panel)); box-shadow: 0 0 0 1px var(--accent); }
.ex-choice.on::after { content: ""; position: absolute; top: var(--sp-2); right: var(--sp-2); width: 6px; height: 6px; border-radius: var(--r-full); background: var(--accent); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 25%, transparent); }
.ex-choice:focus-within { outline: 2px solid var(--accent); outline-offset: 1px; }
.ex-choice input { position: absolute; opacity: 0; inset: 0; margin: 0; cursor: pointer; }
.ex-choice b { font-size: var(--fs-sm); font-weight: 600; }
.ex-choice small { display: none; }
.ex-choice.on small { display: block; color: var(--text-2); font-size: var(--fs-xs); line-height: var(--lh); margin-top: 2px; }
.ex-path { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
/* 应用内文件选择（R4）：隐藏原生 input，用按钮触发并展示已选文件名 */
.ex-file-input { display: none; }
.ex-file-label { font-size: var(--fs-sm); color: var(--text-2); }
.ex-file-row { display: flex; align-items: center; gap: var(--sp-2); min-width: 0; flex-wrap: wrap; }
.ex-file-name { min-width: 0; flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: var(--fs-xs); color: var(--text-2); }
.ex-file-name.none { color: var(--text-3); }

/* 映射表与预览井 */
.map-table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.map-table th { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line-2); text-align: left; color: var(--text-3); font-size: var(--fs-xs); font-weight: 600; }
.map-table td { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); text-align: left; }
.map-table td:first-child { font-family: var(--mono); font-size: var(--fs-xs); }
.map-table td small { color: var(--err); font-size: var(--fs-xs); }
.map-table td small.req { margin-inline-start: var(--sp-2); }
.preview-scroll { max-height: 180px; overflow: auto; padding: var(--sp-1) var(--sp-2); border: 1px solid var(--stroke); border-radius: var(--r); background: var(--surface-sunken); box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 30%, transparent); }
.preview-scroll .map-table td, .preview-scroll .map-table th { border-bottom-color: var(--line); }
.grow { flex: 1; }
</style>
