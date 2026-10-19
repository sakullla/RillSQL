<script setup lang="ts">
import { computed, ref, watch } from "vue";
import type { ListboxOption } from "../ui/types";
import Listbox from "../ui/Listbox.vue";
import { renderCsv, type CsvCell, type CsvDelimiter, type CsvOptions, type NullToken } from "./csv";
import { renderInsert, type InsertTarget } from "./insert";
import { renderJson } from "./json";
import { planExport, type ExportOrigin, type ExportScope } from "./plan";
import { chooseExportPath } from "./native";
import type { ExportFormat } from "./write";

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
  { value: ",", label: "逗号 ," },
  { value: ";", label: "分号 ;" },
  { value: "\t", label: "制表符" },
];
const nullOptions: ListboxOption[] = [
  { value: "", label: "空字段（无引号）" },
  { value: "\\N", label: "\\N" },
  { value: "NULL", label: "NULL（无引号）" },
];
const encodingOptions: ListboxOption[] = [
  { value: "utf8", label: "UTF-8" },
  { value: "bom", label: "UTF-8 + BOM（Excel 打开不乱码）" },
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

watch(format, (next, previous) => {
  if (!props.open || next === previous) return;
  const extension = next === "json" ? "json" : next === "insert" ? "sql" : "csv";
  path.value = path.value.replace(/\.(csv|json|sql)$/i, `.${extension}`);
  if (!/\.(csv|json|sql)$/i.test(path.value)) path.value = `${path.value}.${extension}`;
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
const preview = computed(() => {
  const rows = props.previewRows.slice(0, 4);
  if (format.value === "json") return renderJson(props.columns, rows);
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
    error.value = `无法打开保存对话框：${cause instanceof Error ? cause.message : String(cause)}`;
  } finally {
    choosing.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="ex-overlay" role="presentation">
    <div class="ex-sheet" role="dialog" aria-modal="true" :aria-label="format === 'json' ? '导出为 JSON' : format === 'insert' ? '导出为 INSERT' : '导出为 CSV'">
      <header class="ex-head">
        <div class="ex-titles">
          <h2>{{ format === "json" ? "导出为 JSON" : format === "insert" ? "导出为 INSERT" : "导出为 CSV" }}</h2>
          <p>这只是数据导出，不是数据库备份；要备份请用数据库自带的备份工具。</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>

      <div class="ex-body">
        <p v-if="error" class="ex-alert" role="alert">{{ error }}</p>
        <fieldset class="ex-options" :disabled="choosing">
        <p v-if="pending" class="ex-note warn">表格里还有 {{ pending }} 处更改没有提交。导出的是数据库里已提交的数据，不包含这些未提交的更改。</p>

        <fieldset class="ex-scope">
          <legend>导出成哪种文件</legend>
          <label class="ex-choice" :class="{ on: format === 'csv' }">
            <input v-model="format" type="radio" name="ex-format" value="csv" />
            <span><b>CSV</b><small>表格文本。空值按下面的 NULL 写法写出。</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'json' }">
            <input v-model="format" type="radio" name="ex-format" value="json" />
            <span><b>JSON 数组</b><small>每个对象是一行。空值仍是 null，空字符串仍是空字符串。</small></span>
          </label>
          <label class="ex-choice" :class="{ on: format === 'insert' }">
            <input v-model="format" type="radio" name="ex-format" value="insert" />
            <span><b>INSERT 语句</b><small>只包含这些行，不含建表或整库脚本。空值写成 NULL。</small></span>
          </label>
        </fieldset>
        <p v-if="insertBlocked" class="ex-alert" role="alert">{{ insertReason || "还不能确定 INSERT 要写入的表，不能开始。" }}</p>

        <fieldset class="ex-scope">
          <legend>导出哪些行</legend>
          <label class="ex-choice" :class="{ on: scope === 'loaded' }">
            <input v-model="scope" type="radio" name="ex-scope" value="loaded" />
            <span>
              <b>只导出已读取的 {{ loadedCount }} 行</b>
              <small>{{ truncated ? "结果还没读完：没读出来的行不会写进文件，文件名会标注「（部分）」。" : "表格里已经读出了这次结果的全部行。" }}</small>
            </span>
          </label>
          <label class="ex-choice" :class="{ on: scope === 'full' }">
            <input v-model="scope" type="radio" name="ex-scope" value="full" />
            <span>
              <b>{{ origin === "query" ? "重新执行这条查询，导出全部结果" : "导出符合当前筛选的全部行" }}</b>
              <small>{{ origin === "query" ? "只有单条 SELECT 可以重新执行；会一边读取一边写入文件。" : "由数据库读取并直接写入文件，不会先把全部行装进表格。" }}</small>
            </span>
          </label>
        </fieldset>

        <p v-if="plan.notice" class="ex-note" data-export-notice>{{ plan.notice }}</p>
        <p v-if="plan.rejectReason" class="ex-alert" role="alert">{{ plan.rejectReason }}</p>
        <label v-if="plan.reexecutes" class="ex-ack">
          <input v-model="acknowledged" type="checkbox" />
          我明白：导出会重新执行这条 SELECT，结果可能和当前表格看到的不一样
        </label>

        <label class="ex-path">
          保存到
          <input v-model="path" aria-label="导出路径" spellcheck="false" />
        </label>

        <div class="ex-preview-wrap">
          <p v-if="format === 'csv'" class="ex-note">预览（前 4 行）：非 NULL 的值都会加引号，所以 "" 是空字符串，{{ nullToken === "" ? "空字段" : nullToken }} 是 NULL。</p>
          <p v-else class="ex-note">预览（前 4 行）。空值保持为空值。未提交的修改不会进入文件。</p>
          <pre class="ex-preview">{{ preview }}</pre>
        </div>

        <details v-if="format === 'csv'" class="disclosure">
          <summary>高级选项 <span class="muted">分隔符、NULL 写法、编码、列名行</span></summary>
          <div class="disclosure-body ex-advanced">
            <div class="ex-field">
              <label for="export-delimiter">分隔符</label>
              <Listbox id="export-delimiter" :model-value="delimiter" :options="delimiterOptions" aria-label="分隔符" :disabled="choosing" @update:model-value="onDelimiter" />
            </div>
            <div class="ex-field">
              <label for="export-null">NULL 写为</label>
              <Listbox id="export-null" :model-value="nullToken" :options="nullOptions" aria-label="NULL 写法" :disabled="choosing" @update:model-value="onNullToken" />
            </div>
            <div class="ex-field">
              <label for="export-encoding">编码</label>
              <Listbox id="export-encoding" :model-value="bom ? 'bom' : 'utf8'" :options="encodingOptions" aria-label="编码" :disabled="choosing" @update:model-value="onEncoding" />
            </div>
            <label class="ex-header-row"><input v-model="header" type="checkbox" /> 第一行写入列名</label>
          </div>
        </details>
        </fieldset>
      </div>

      <footer class="ex-foot">
        <span class="muted sm">{{ plan.label }}{{ plan.partial ? " · 不是完整结果" : "" }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">取消</button>
        <button class="btn primary" type="button" :disabled="!canStart || choosing" @click="start">{{ choosing ? "选择保存位置…" : "导出" }}</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.ex-overlay { position: absolute; inset: 0; background: var(--overlay); display: grid; place-items: center; z-index: 5; padding: var(--sp-4); }
.ex-sheet { width: min(720px, 100%); max-height: min(88vh, 880px); display: flex; flex-direction: column; background: var(--panel); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--line-2); box-shadow: var(--shadow-lg); }
.ex-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line); }
.ex-titles { flex: 1; min-width: 0; }
.ex-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; }
.ex-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); }
.ex-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-advanced :deep(.listbox) { width: 100%; }
.ex-advanced :deep(.listbox-trigger) { width: 100%; justify-content: space-between; }
.ex-options { margin: 0; padding: 0; border: 0; min-width: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line); background: var(--panel-2); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.ex-note { margin: 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.ex-note.warn { padding: var(--sp-2) var(--sp-3); border: 1px solid var(--warn-line); border-radius: var(--r); background: var(--warn-soft); color: var(--warn); }
.ex-alert { margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); font-size: var(--fs-sm); }
.ex-scope { margin: 0; padding: 0; border: 0; display: flex; flex-direction: column; gap: var(--sp-2); }
.ex-scope legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-sm); font-weight: 600; }
.ex-choice { display: flex; gap: var(--sp-2); align-items: flex-start; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r-md); cursor: pointer; transition: border-color var(--dur-fast) var(--ease), background var(--dur-fast) var(--ease); }
.ex-choice:hover { background: var(--hover); }
.ex-choice.on { border-color: var(--accent); background: var(--accent-soft); }
.ex-choice input { margin-top: 3px; }
.ex-choice b { font-size: var(--fs-sm); font-weight: 600; }
.ex-choice small { display: block; color: var(--text-2); font-size: var(--fs-xs); line-height: var(--lh); margin-top: 2px; }
.ex-ack { display: flex; gap: var(--sp-2); align-items: flex-start; font-size: var(--fs-sm); color: var(--text); }
.ex-path { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.ex-path input { width: 100%; }
.ex-preview { margin: var(--sp-1) 0 0; max-height: 140px; overflow: auto; background: var(--sunken); padding: var(--sp-2) var(--sp-3); border-radius: var(--r); font-family: var(--mono); font-size: var(--fs-xs); line-height: var(--lh); white-space: pre-wrap; word-break: break-all; }
.ex-advanced { display: flex; gap: var(--sp-3); flex-wrap: wrap; align-items: flex-end; }
.ex-field { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-xs); color: var(--text-2); }
.ex-advanced label.ex-header-row { flex-direction: row; align-items: center; gap: var(--sp-2); font-size: var(--fs-sm); color: var(--text); }
</style>
