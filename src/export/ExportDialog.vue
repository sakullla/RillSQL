<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { renderCsv, type CsvCell, type CsvDelimiter, type CsvOptions, type NullToken } from "./csv";
import { planExport, type ExportOrigin, type ExportScope } from "./plan";

const props = defineProps<{
  open: boolean;
  origin: ExportOrigin;
  statement?: string;
  truncated: boolean;
  fileStem: string;
  loadedCount: number;
  pending: number;
  columns: string[];
  previewRows: Record<string, CsvCell>[];
}>();

const emit = defineEmits<{
  close: [];
  start: [payload: { planNotice: string; scope: ExportScope; acknowledged: boolean; path: string; csv: CsvOptions }];
}>();

const scope = ref<ExportScope>("loaded");
const delimiter = ref<CsvDelimiter>(",");
const nullToken = ref<NullToken>("");
const bom = ref(true);
const header = ref(true);
const path = ref("");
const acknowledged = ref(false);

watch(
  () => props.open,
  (open) => {
    if (!open) return;
    scope.value = "loaded";
    acknowledged.value = false;
    const stamp = new Date().toISOString().slice(0, 10);
    path.value = `${props.fileStem}-${stamp}.csv`;
  },
);

const csv = computed<CsvOptions>(() => ({ delimiter: delimiter.value, nullToken: nullToken.value, bom: bom.value, header: header.value }));
const plan = computed(() =>
  planExport({
    origin: props.origin,
    scope: scope.value,
    statement: props.statement,
    truncated: props.truncated,
    fileName: path.value.trim() || `${props.fileStem}.csv`,
  }),
);
const preview = computed(() => renderCsv(props.columns, props.previewRows.slice(0, 4), { ...csv.value, bom: false }));
const canStart = computed(() => plan.value.allowed && (!plan.value.reexecutes || acknowledged.value) && path.value.trim().length > 0);

function start() {
  if (!canStart.value) return;
  path.value = plan.value.fileName;
  emit("start", {
    planNotice: plan.value.notice,
    scope: scope.value,
    acknowledged: acknowledged.value,
    path: plan.value.fileName,
    csv: { ...csv.value },
  });
}
</script>

<template>
  <div v-if="open" class="overlay" role="presentation">
    <div class="sheet" role="dialog" aria-modal="true" aria-label="导出 CSV">
      <header>
        <div>
          <h2>导出 CSV</h2>
          <p class="hint">CSV 是数据导出，不是数据库备份。</p>
        </div>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">关闭</button>
      </header>
      <div class="body">
        <p v-if="pending" class="hint">有 {{ pending }} 项未提交的变更。导出的是已提交数据，不包含暂存区。</p>
        <label class="choice" :class="{ on: scope === 'loaded' }">
          <input v-model="scope" type="radio" name="ex-scope" value="loaded" />
          <span>
            <b>仅已读取的 {{ loadedCount }} 行</b>
            <small>{{ truncated ? "不是完整结果：未读取的行不会写入，文件名会注明“（部分）”。" : "已读取全部当前结果。" }}</small>
          </span>
        </label>
        <label class="choice" :class="{ on: scope === 'full' }">
          <input v-model="scope" type="radio" name="ex-scope" value="full" />
          <span>
            <b>{{ origin === "query" ? "重新执行这条查询，导出完整结果" : "按当前条件导出全部匹配行" }}</b>
            <small>{{ origin === "query" ? "只有 SELECT 可以重新执行导出。" : "服务器端读取并流式写入，不会把全部行先填进表格。" }}</small>
          </span>
        </label>
        <p v-if="plan.notice" class="hint" data-export-notice>{{ plan.notice }}</p>
        <p v-if="plan.rejectReason" class="grid-alert" role="alert">{{ plan.rejectReason }}</p>
        <label v-if="plan.reexecutes" class="hint">
          <input v-model="acknowledged" type="checkbox" />
          我知道导出会重新执行这条 SELECT
        </label>
        <div class="form-row">
          <label>分隔符
            <select v-model="delimiter" aria-label="分隔符">
              <option value=",">逗号 ,</option>
              <option value=";">分号 ;</option>
              <option value="\t">制表符</option>
            </select>
          </label>
          <label>NULL 写为
            <select v-model="nullToken" aria-label="NULL 写法">
              <option value="">空字段（无引号）</option>
              <option value="\N">\N</option>
              <option value="NULL">NULL（无引号）</option>
            </select>
          </label>
          <label>编码
            <select v-model="bom" aria-label="编码">
              <option :value="false">UTF-8</option>
              <option :value="true">UTF-8 + BOM（Excel）</option>
            </select>
          </label>
        </div>
        <label class="hint"><input v-model="header" type="checkbox" /> 第一行写入列名</label>
        <label class="hint">保存到
          <input v-model="path" aria-label="导出路径" spellcheck="false" />
        </label>
        <p class="hint">非 NULL 值一律加引号，因此 "" 是空字符串，{{ nullToken === "" ? "空字段" : nullToken }} 是 NULL。</p>
        <pre class="ex-preview">{{ preview }}</pre>
      </div>
      <footer>
        <span class="hint">{{ plan.label }}{{ plan.partial ? " · 不是完整结果" : "" }}</span>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('close')">取消</button>
        <button class="btn primary" type="button" :disabled="!canStart" @click="start">导出</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.overlay { position: absolute; inset: 0; background: rgba(8, 18, 15, 0.45); display: grid; place-items: center; z-index: 5; padding: 16px; }
.sheet { width: min(780px, 100%); max-height: min(88vh, 880px); overflow: auto; background: var(--panel); color: var(--text); border-radius: 14px; border: 1px solid var(--line-2); box-shadow: var(--shadow-lg); }
.sheet h2 { margin: 0; font-size: 16px; }
.sheet header, .sheet footer { display: flex; gap: 8px; align-items: center; padding: 12px 16px; }
.sheet header { border-bottom: 1px solid var(--line); }
.sheet footer { border-top: 1px solid var(--line); background: var(--panel-2); }
.sheet .body { padding: 14px 16px; display: flex; flex-direction: column; gap: 10px; }
.choice { display: flex; gap: 8px; padding: 8px; border: 1px solid var(--line); border-radius: 8px; }
.choice.on { border-color: var(--accent); background: var(--accent-soft); }
.hint { color: var(--text-2); }
.hint small, .choice small { display: block; color: var(--text-2); }
.form-row { display: flex; gap: 8px; flex-wrap: wrap; }
.form-row label, .hint { display: flex; flex-direction: column; gap: 4px; font-size: 12.5px; }
.ex-preview { margin: 0; max-height: 140px; overflow: auto; background: var(--sunken); padding: 8px; border-radius: 6px; font-family: var(--mono); font-size: 12px; white-space: pre-wrap; }
.grid-alert { margin: 0; padding: 8px; background: var(--err-soft); color: var(--err); border-radius: 6px; }
.grow { flex: 1; }
</style>
