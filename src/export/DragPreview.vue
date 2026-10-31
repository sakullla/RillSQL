<script setup lang="ts">
import { computed } from "vue";
import { showCell } from "../grid/display";
import type { Cell } from "../grid/types";
import { formatDropBytes, previewRowsWithin, unsupportedDropText, type DroppedFileState } from "./dropped";
import type { ImportSeed } from "./import";

const props = defineProps<{
  open: boolean;
  state: DroppedFileState | null;
  /** 预览行预算（字节）：由 previewKB 设置换算，见 previewBudgetBytes。 */
  budgetBytes: number;
}>();

const emit = defineEmits<{ cancel: []; proceed: [seed: ImportSeed] }>();

const ready = computed(() => (props.state?.kind === "ready" ? props.state : null));
const notice = computed(() => {
  if (!props.state) return "";
  if (props.state.kind === "unsupported") return unsupportedDropText(props.state.extension);
  if (props.state.kind === "error") return props.state.message;
  return "";
});

const visible = computed(() => (ready.value ? previewRowsWithin(ready.value.parsed.rows, props.budgetBytes) : { rows: [] as Record<string, Cell>[], hidden: 0 }));
const columns = computed(() => ready.value?.parsed.columns ?? []);
const formatLabel = computed(() => (ready.value?.preview.format === "json" ? "JSON 数组" : "CSV"));

/** 继续导入：把已读取的文本交给现有导入对话框的列映射流程重新解析；预览本身零写入。 */
function proceed() {
  if (!ready.value) return;
  emit("proceed", {
    format: ready.value.preview.format,
    text: ready.value.preview.text,
    fileName: ready.value.fileName,
  });
}
</script>

<template>
  <div v-if="open && state" class="dp-overlay" role="presentation">
    <div class="dp-sheet" role="dialog" aria-modal="true" aria-label="拖放文件预览">
      <header class="dp-head">
        <div class="dp-titles">
          <h2>拖放文件预览</h2>
          <p>先看前几行再决定。取消不会写入任何数据；继续才会进入导入对话框的列映射。</p>
        </div>
        <button class="icon-btn" type="button" aria-label="关闭" @click="emit('cancel')">
          <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </header>
      <div class="dp-body">
        <p v-if="notice" class="dp-alert" role="alert" data-drop-notice>{{ notice }}</p>
        <template v-if="ready">
          <p class="dp-meta" data-drop-meta><b class="mono">{{ ready.fileName }}</b> · {{ formatLabel }} · {{ formatDropBytes(ready.preview.fileSize) }}</p>
          <p v-if="ready.preview.truncated" class="dp-alert" role="note" data-drop-truncated>
            文件有 {{ formatDropBytes(ready.preview.fileSize) }}，超过 2 MB 预览读取上限：只读取了前 {{ formatDropBytes(ready.preview.readBytes) }}，继续导入也只会写入已读取的这些行。
          </p>
          <p class="dp-note" data-drop-count>解析出 {{ ready.parsed.rows.length }} 行，显示前 {{ visible.rows.length }} 行{{ visible.hidden > 0 ? `（其余 ${visible.hidden} 行已折叠，导入不受影响）` : "" }}。</p>
          <div class="dp-scroll">
            <table class="dp-table">
              <thead><tr><th v-for="column in columns" :key="column">{{ column }}</th></tr></thead>
              <tbody>
                <tr v-for="(row, index) in visible.rows" :key="index">
                  <td v-for="column in columns" :key="column">{{ showCell(row[column] ?? null).label }}</td>
                </tr>
              </tbody>
            </table>
          </div>
        </template>
      </div>
      <footer class="dp-foot">
        <span class="muted sm">预览阶段不写入任何数据</span>
        <span class="grow"></span>
        <button class="btn" type="button" data-action="drop-cancel" @click="emit('cancel')">取消</button>
        <button v-if="ready" class="btn primary" type="button" data-action="drop-continue" @click="proceed">继续导入</button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
/* 与导入/迁移对话框同一套 overlay 表面体系。 */
.dp-overlay { position: fixed; inset: 0; background: var(--overlay); backdrop-filter: blur(3px); display: grid; place-items: center; z-index: var(--z-dialog); padding: var(--sp-4); }
.dp-sheet { width: min(680px, 100%); max-height: min(88vh, 760px); display: flex; flex-direction: column; background: var(--surface-overlay); color: var(--text); border-radius: var(--r-xl); border: 1px solid var(--stroke-strong); box-shadow: var(--shadow-lg); animation: modal-in var(--dur) var(--ease-out); }
.dp-head { display: flex; align-items: flex-start; gap: var(--sp-3); padding: var(--sp-4) var(--sp-5) var(--sp-3); border-bottom: 1px solid var(--line-2); }
.dp-titles { flex: 1; min-width: 0; }
.dp-titles h2 { margin: 0; font-size: var(--fs-lg); font-weight: 650; letter-spacing: -0.01em; }
.dp-titles p { margin: var(--sp-1) 0 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.dp-body { padding: var(--sp-4) var(--sp-5); overflow: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: var(--sp-3); }
.dp-foot { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-3) var(--sp-5); border-top: 1px solid var(--line-2); background: color-mix(in srgb, var(--surface-overlay) 94%, var(--text) 6%); border-radius: 0 0 var(--r-xl) var(--r-xl); }
.dp-meta { display: flex; align-items: center; gap: var(--sp-2); flex-wrap: wrap; margin: 0; padding: var(--sp-2) var(--sp-3); border: 1px solid var(--stroke); border-radius: var(--r-md); background: var(--surface-panel); box-shadow: var(--shadow-sm); font-size: var(--fs-sm); color: var(--text-2); }
.dp-meta b { font-weight: 600; color: var(--text); font-size: var(--fs-xs); }
.dp-note { margin: 0; font-size: var(--fs-sm); color: var(--text-2); line-height: var(--lh); }
.dp-alert { display: flex; align-items: flex-start; gap: var(--sp-2); margin: 0; padding: var(--sp-2) var(--sp-3); background: var(--err-soft); color: var(--err); border: 1px solid var(--err-line); border-radius: var(--r); box-shadow: inset 3px 0 0 var(--err); font-size: var(--fs-sm); line-height: var(--lh); }
.dp-scroll { max-height: 260px; overflow: auto; padding: var(--sp-1) var(--sp-2); border: 1px solid var(--stroke); border-radius: var(--r); background: var(--surface-sunken); box-shadow: inset 0 1px 2px color-mix(in srgb, var(--press) 30%, transparent); }
.dp-table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.dp-table th { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line-2); text-align: left; color: var(--text-3); font-size: var(--fs-xs); font-weight: 600; font-family: var(--mono); }
.dp-table td { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); text-align: left; }
.grow { flex: 1; }
</style>
