<script setup lang="ts">
import { computed, ref, watch } from "vue";
import Listbox from "../ui/Listbox.vue";
import type { ListboxOption } from "../ui/types";
import { catalogConnections } from "../catalog/store";
import { gridCommit, gridDescribe } from "../grid/api";
import type { TableTarget } from "../grid/target";
import type { TableSchema } from "../grid/types";
import { useRowWindow } from "../grid/virtual";
import { showCell } from "../grid/display";
import {
  importChanges,
  importResultText,
  mappedPreview,
  parseImportCsv,
  parseImportJson,
  requiredGaps,
  suggestMapping,
  type ParsedImport,
} from "./import";

const props = defineProps<{
  open: boolean;
  target: TableTarget;
  schema: TableSchema;
  tables?: { schema: string; table: string }[];
}>();

const emit = defineEmits<{
  close: [];
  done: [message: string];
}>();

const format = ref<"csv" | "json">("csv");
const tableKey = ref("");
const localSchema = ref<TableSchema | null>(null);
const localTarget = ref<TableTarget | null>(null);
const parsed = ref<ParsedImport | null>(null);
const mapping = ref<Record<string, string>>({});
const parseError = ref("");
const resultText = ref("");
const writing = ref(false);
const describing = ref(false);

const choices = computed(() => {
  const list = [...(props.tables ?? [])];
  if (!list.some((item) => item.schema === props.target.schema && item.table === props.target.table)) {
    list.unshift({ schema: props.target.schema, table: props.target.table });
  }
  return list;
});

function keyOf(schema: string, table: string): string {
  return `${schema}\u0000${table}`;
}

const tableOptions = computed<ListboxOption[]>(() =>
  choices.value.map((item) => ({
    value: keyOf(item.schema, item.table),
    label: item.schema ? `${item.schema}.${item.table}` : item.table,
  })),
);
const mappingOptions = computed<ListboxOption[]>(() => [
  { value: "", label: "不导入" },
  ...(parsed.value?.columns ?? []).map((name) => ({ value: name, label: name })),
]);

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    format.value = "csv";
    parsed.value = null;
    mapping.value = {};
    parseError.value = "";
    resultText.value = "";
    writing.value = false;
    localSchema.value = props.schema;
    localTarget.value = props.target;
    tableKey.value = keyOf(props.target.schema, props.target.table);
  },
  { immediate: true },
);

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
    if (described.kind !== "table") parseError.value = "只有数据表可以导入，不能从文件建表，也不能写入视图。";
    if (parsed.value) mapping.value = suggestMapping(described.columns, parsed.value.columns);
  } catch (error) {
    parseError.value = error instanceof Error ? error.message : String(error);
  } finally {
    describing.value = false;
  }
}

function onTablePick(value: string) {
  if (writing.value) return;
  tableKey.value = value;
  void chooseTable();
}

async function onFile(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0];
  resultText.value = "";
  parsed.value = null;
  if (!file) return;
  const text = await file.text();
  const outcome = format.value === "json" ? parseImportJson(text) : parseImportCsv(text);
  if (!outcome.ok) {
    parseError.value = outcome.reason;
    mapping.value = {};
    return;
  }
  parseError.value = "";
  parsed.value = outcome.data;
  mapping.value = suggestMapping(activeSchema.value.columns, outcome.data.columns);
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
    resultText.value = `导入没有完成：${error instanceof Error ? error.message : String(error)}。没有把未写入的行记成成功。`;
  } finally {
    writing.value = false;
  }
}
</script>

<template>
  <div v-if="open" class="ex-overlay" role="presentation">
    <div class="ex-sheet" role="dialog" aria-modal="true" aria-label="导入到已有表">
      <header class="ex-head">
        <div class="ex-titles">
          <h2>导入到已有表</h2>
          <p>只把文件里的行插入一张已有的表。不会建表，也不会把表上还没提交的修改写进去。</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="emit('close')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>
      <div class="ex-body">
        <p class="ex-note" data-import-target>目标：{{ connectionName }} / {{ objectName }} <span v-if="environmentLabel" class="env xs" :class="environmentTone !== 'none' ? `env-${environmentTone}` : undefined">{{ environmentLabel }}</span></p>
        <p v-if="parseError" class="ex-alert" role="alert">{{ parseError }}</p>
        <p v-if="resultText" class="ex-note" role="status" data-import-result>{{ resultText }}</p>
        <div class="ex-path">导入到
          <span class="im-lock" :class="{ locked: writing }" :aria-disabled="writing ? 'true' : undefined">
            <Listbox
              :model-value="tableKey"
              :options="tableOptions"
              aria-label="导入的表"
              @update:model-value="onTablePick"
            />
          </span>
        </div>
        <fieldset class="ex-scope">
          <legend>文件格式</legend>
          <label class="ex-choice" :class="{ on: format === 'csv' }"><input v-model="format" type="radio" name="im-format" value="csv" :disabled="writing" /><span><b>CSV</b><small>第一行是列名。没有引号的空字段是空值。</small></span></label>
          <label class="ex-choice" :class="{ on: format === 'json' }"><input v-model="format" type="radio" name="im-format" value="json" :disabled="writing" /><span><b>JSON 数组</b><small>必须是对象数组，确认前就会拒绝其他形状。</small></span></label>
        </fieldset>
        <label class="ex-path">选择文件
          <input type="file" aria-label="导入文件" accept=".csv,.json,.txt,text/csv,application/json" :disabled="writing" @change="onFile" />
        </label>
        <p v-if="parsed && gaps.length" class="ex-alert" role="alert">必填字段 {{ gaps.join("、") }} 没有对应列，不能开始写入。</p>
        <table v-if="parsed" class="map-table">
          <thead><tr><th>表字段</th><th>文件列</th></tr></thead>
          <tbody>
            <tr v-for="column in activeSchema.columns" :key="column.name">
              <td>{{ column.name }}<small v-if="!column.nullable && !column.auto"> 必填</small></td>
              <td>
                <span class="im-lock" :class="{ locked: writing }" :aria-disabled="writing ? 'true' : undefined">
                  <Listbox
                    v-model="mapping[column.name]"
                    :options="mappingOptions"
                    :aria-label="`${column.name} 对应的列`"
                  />
                </span>
              </td>
            </tr>
          </tbody>
        </table>
        <p v-if="parsed" class="ex-note">将写入 {{ parsed.rows.length }} 行。下面是对应好的字段，取消则不会写入。</p>
        <div v-if="parsed" ref="viewport" class="preview-scroll" @scroll.passive="onScroll">
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
        <span class="muted sm">{{ describing ? "正在读取表结构…" : writing ? "正在逐行写入…" : "确认后才写入" }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" :disabled="writing" @click="emit('close')">取消</button>
        <button class="btn primary" type="button" :disabled="!canWrite" @click="confirm">导入</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.ex-overlay { position: absolute; inset: 0; background: rgba(12, 16, 24, 0.45); display: grid; place-items: center; z-index: 5; padding: var(--sp-4); }
.ex-sheet { width: min(720px, 100%); max-height: min(88vh, 880px); display: flex; flex-direction: column; background: var(--panel); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--line-2); box-shadow: var(--shadow-lg); }
.ex-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line); }
.ex-titles { flex: 1; min-width: 0; }
.ex-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; }
.ex-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); }
.ex-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.ex-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line); background: var(--panel-2); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.ex-note { margin: 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.ex-alert { margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); font-size: var(--fs-sm); }
.ex-scope { margin: 0; padding: 0; border: 0; display: flex; flex-direction: column; gap: var(--sp-2); }
.ex-scope legend { padding: 0; margin-bottom: var(--sp-1); font-size: var(--fs-sm); font-weight: 600; }
.ex-choice { display: flex; gap: var(--sp-2); align-items: flex-start; padding: var(--sp-3); border: 1px solid var(--line); border-radius: var(--r-md); }
.ex-choice.on { border-color: var(--accent); background: var(--accent-soft); }
.ex-choice b { font-size: var(--fs-sm); }
.ex-choice small { display: block; color: var(--text-2); font-size: var(--fs-xs); }
.ex-path { display: flex; flex-direction: column; gap: var(--sp-1); font-size: var(--fs-sm); color: var(--text-2); }
.im-lock { display: inline-flex; max-width: 100%; min-width: 0; }
.im-lock.locked { pointer-events: none; opacity: 0.6; }
.map-table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.map-table th, .map-table td { text-align: left; padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); }
.preview-scroll { max-height: 180px; overflow: auto; }
.grow { flex: 1; }
</style>
