<script setup lang="ts">
import { computed, ref } from "vue";
import DumpDialog from "./DumpDialog.vue";
import MigrateDialog from "./MigrateDialog.vue";
import { cancelTask, clearFinishedTasks, exportTasks, taskStatusLabel, taskStatusText, type ExportTask } from "./tasks";

const open = ref(false);
const dumpOpen = ref(false);
const migrateOpen = ref(false);
const running = computed(() => exportTasks.find((task) => task.status === "running"));
const hasFinished = computed(() => exportTasks.some((task) => task.status !== "running"));
const label = computed(() => {
  const task = running.value;
  if (!task) return "任务";
  const verb = task.kind === "migrate" ? "迁移中" : "导出中";
  return task.totalRows > 0 ? `${verb} ${task.doneRows}/${task.totalRows}` : `${verb} ${task.doneRows} 行`;
});

function cancel(task: ExportTask) {
  void cancelTask(task);
}

/** 迁移报告的失败标识最多展示前 10 个，其余以计数说明。 */
function failedSample(task: ExportTask): { ids: string[]; more: number } {
  const ids = task.report?.failedIds ?? [];
  return { ids: ids.slice(0, 10), more: Math.max(0, ids.length - 10) };
}

function badgeVariant(status: ExportTask["status"]): string {
  if (status === "done") return "ok";
  if (status === "failed") return "err";
  if (status === "running") return "info";
  return "warn";
}

function percent(task: ExportTask): number {
  return Math.floor((task.doneRows / Math.max(1, task.totalRows)) * 100);
}
</script>

<template>
  <div class="task-center" data-module="export">
    <button class="tc-trigger" :class="{ busy: running }" type="button" aria-label="后台任务" :aria-expanded="open" @click="open = !open">
      <svg class="i" viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2v6l3 2" /><circle cx="8" cy="8" r="6.2" /></svg>
      {{ label }}
    </button>
    <div v-if="open" class="tc-pop" role="dialog" aria-label="任务列表">
      <div class="tc-head">
        <strong>后台任务</strong>
        <span class="grow"></span>
        <button class="btn sm ghost" type="button" data-action="dump-open" @click="dumpOpen = true">全库导出</button>
        <button class="btn sm ghost" type="button" data-action="migrate-open" @click="migrateOpen = true">跨库迁移</button>
        <button class="btn sm ghost" type="button" data-action="clear-finished" :disabled="!hasFinished" @click="clearFinishedTasks">清除已结束的</button>
      </div>
      <p v-if="!exportTasks.length" class="tc-empty">没有后台任务。</p>
      <article v-for="task in exportTasks" :key="task.id" class="tc-task" :data-status="task.status">
        <div class="tc-task-top">
          <span class="badge" :class="badgeVariant(task.status)">{{ taskStatusLabel(task.status) }}</span>
          <span class="tc-name" :title="task.path">{{ task.fileName }}</span>
          <span class="grow"></span>
          <button v-if="task.status === 'running' && task.cancellable !== false" class="btn sm" type="button" @click="cancel(task)">取消</button>
        </div>
        <div
          v-if="task.status === 'running'"
          class="tc-progress"
          role="progressbar"
          aria-valuemin="0"
          :aria-valuemax="Math.max(1, task.totalRows)"
          :aria-valuenow="task.doneRows"
          :aria-label="`导出 ${task.fileName} 的进度`"
        >
          <i :style="{ width: `${percent(task)}%` }"></i>
        </div>
        <p class="tc-status">{{ taskStatusText(task) }}</p>
        <details v-if="task.report && (task.report.failedIds.length > 0 || task.report.firstErrors.length > 0)" class="tc-report">
          <summary>失败明细（{{ task.report.failedIds.length }} 个标识）</summary>
          <ul class="tc-failed">
            <li v-for="id in failedSample(task).ids" :key="id">{{ id }}</li>
          </ul>
          <p v-if="failedSample(task).more > 0" class="tc-more">还有 {{ failedSample(task).more }} 个未展示。</p>
          <ul class="tc-errors">
            <li v-for="item in task.report.firstErrors.slice(0, 5)" :key="item">{{ item }}</li>
          </ul>
        </details>
      </article>
    </div>
    <DumpDialog :open="dumpOpen" @close="dumpOpen = false" />
    <MigrateDialog :open="migrateOpen" @close="migrateOpen = false" />
  </div>
</template>

<style scoped>
/* 任务中心：触发器是 chrome 上的下沉胶囊，列表是 overlay 表面浮层（同菜单体系）。 */
.task-center { position: relative; }
.tc-trigger { white-space: nowrap; display: inline-flex; align-items: center; gap: var(--sp-1); height: var(--control-h-sm); padding: 0 var(--sp-3); border-radius: var(--r-full); background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); color: var(--text-2); font-size: var(--fs-xs); font-variant-numeric: tabular-nums; transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease); }
.tc-trigger:hover { background: var(--hover); color: var(--text); }
.tc-trigger:active { box-shadow: inset 0 0 0 1px var(--stroke), inset 0 1px 2px var(--press); }
.tc-trigger .i { width: 13px; height: 13px; }
.tc-trigger.busy { background: var(--accent-soft); color: var(--accent-text); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--accent) 26%, transparent); }
.tc-pop { position: absolute; right: 0; top: calc(100% + var(--sp-1)); width: 340px; max-height: 420px; overflow: auto; background: var(--surface-overlay); border: 1px solid var(--stroke-strong); border-radius: var(--r-md); box-shadow: var(--shadow); padding: var(--sp-2) var(--sp-3); z-index: var(--z-menu); animation: pop-in var(--dur-fast) var(--ease-out); }
.tc-head { display: flex; align-items: center; gap: var(--sp-2); padding: var(--sp-1) 0 var(--sp-2); border-bottom: 1px solid var(--line-2); }
.tc-head strong { font-size: var(--fs-sm); font-weight: 600; }
.tc-empty { margin: 0; padding: var(--sp-5) 0; color: var(--text-3); font-size: var(--fs-sm); text-align: center; }

/* 任务行：状态语义走 badge 与左缘指示条 */
.tc-task { position: relative; display: flex; flex-direction: column; gap: var(--sp-1); padding: var(--sp-2) 0 var(--sp-2) var(--sp-3); border-bottom: 1px solid var(--line); }
.tc-task:last-child { border-bottom: 0; padding-bottom: var(--sp-1); }
.tc-task::before { content: ""; position: absolute; left: 0; top: var(--sp-2); bottom: var(--sp-2); width: 2px; border-radius: var(--r-full); background: var(--line-2); }
.tc-task[data-status="running"]::before { background: var(--info); }
.tc-task[data-status="done"]::before { background: var(--ok); }
.tc-task[data-status="failed"]::before { background: var(--err); }
.tc-task[data-status="cancelled"]::before { background: var(--warn); }
.tc-task-top { display: flex; align-items: center; gap: var(--sp-2); min-width: 0; }
.tc-name { flex: 1; min-width: 0; font: 500 var(--fs-xs) var(--mono); color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tc-progress { height: 4px; background: var(--surface-sunken); box-shadow: inset 0 0 0 1px var(--stroke); border-radius: var(--r-full); overflow: hidden; }
.tc-progress i { display: block; height: 100%; background: var(--accent); border-radius: var(--r-full); transition: width var(--dur) var(--ease); }
.tc-status { margin: 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
/* 迁移报告：失败标识与首批错误折叠展示，避免长表刷屏。 */
.tc-report { font-size: var(--fs-xs); color: var(--text-2); }
.tc-report summary { cursor: pointer; color: var(--text-3); }
.tc-failed { display: flex; flex-wrap: wrap; gap: var(--sp-1); margin: var(--sp-1) 0 0; padding: 0; list-style: none; }
.tc-failed li { padding: 0 var(--sp-2); border: 1px solid var(--line); border-radius: var(--r-full); font: 500 var(--fs-xs) var(--mono); color: var(--text); }
.tc-more { margin: var(--sp-1) 0 0; color: var(--text-3); }
.tc-errors { margin: var(--sp-1) 0 0; padding-left: var(--sp-4); color: var(--text-3); }
.tc-errors li { margin: 0; }
</style>
