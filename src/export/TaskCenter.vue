<script setup lang="ts">
import { computed, ref } from "vue";
import { cancelRemoteExport, clearFinishedTasks, exportTasks, taskStatusLabel, taskStatusText, type ExportTask } from "./tasks";

const open = ref(false);
const running = computed(() => exportTasks.find((task) => task.status === "running"));
const hasFinished = computed(() => exportTasks.some((task) => task.status !== "running"));
const label = computed(() => {
  const task = running.value;
  if (!task) return "任务";
  return task.totalRows > 0 ? `导出中 ${task.doneRows}/${task.totalRows}` : `导出中 ${task.doneRows} 行`;
});

function cancel(id: string) {
  void cancelRemoteExport(id);
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
        <button class="btn sm ghost" type="button" data-action="clear-finished" :disabled="!hasFinished" @click="clearFinishedTasks">清除已结束的</button>
      </div>
      <p v-if="!exportTasks.length" class="tc-empty">没有后台任务。</p>
      <article v-for="task in exportTasks" :key="task.id" class="tc-task" :data-status="task.status">
        <div class="tc-task-top">
          <span class="badge" :class="badgeVariant(task.status)">{{ taskStatusLabel(task.status) }}</span>
          <span class="tc-name" :title="task.path">{{ task.fileName }}</span>
          <span class="grow"></span>
          <button v-if="task.status === 'running'" class="btn sm" type="button" @click="cancel(task.id)">取消</button>
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
      </article>
    </div>
  </div>
</template>

<style scoped>
.task-center { position: relative; }
.tc-trigger { display: inline-flex; align-items: center; gap: var(--sp-1); height: 26px; padding: 0 var(--sp-3); border-radius: var(--r-full); background: var(--sunken); color: var(--text-2); font-size: var(--fs-xs); transition: background var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease); }
.tc-trigger:hover { background: var(--hover); color: var(--text); }
.tc-trigger .i { width: 13px; height: 13px; }
.tc-trigger.busy { background: var(--accent-soft); color: var(--accent-text); }
.tc-pop { position: absolute; right: 0; top: 32px; width: 340px; max-height: 420px; overflow: auto; background: var(--panel); border: 1px solid var(--line-2); border-radius: var(--r-lg); box-shadow: var(--shadow); padding: var(--sp-3); z-index: 30; }
.tc-head { display: flex; align-items: center; gap: var(--sp-2); margin-bottom: var(--sp-2); }
.tc-head strong { font-size: var(--fs-sm); }
.tc-empty { margin: var(--sp-2) 0; color: var(--text-3); font-size: var(--fs-sm); }
.tc-task { padding: var(--sp-2) 0; border-bottom: 1px solid var(--line); }
.tc-task:last-child { border-bottom: 0; }
.tc-task-top { display: flex; align-items: center; gap: var(--sp-2); }
.tc-name { font-size: var(--fs-sm); font-weight: 500; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.tc-progress { height: 4px; background: var(--sunken); border-radius: var(--r-full); margin: var(--sp-2) 0 var(--sp-1); overflow: hidden; }
.tc-progress i { display: block; height: 100%; background: var(--accent); border-radius: var(--r-full); transition: width var(--dur) var(--ease); }
.tc-status { margin: var(--sp-1) 0 0; font-size: var(--fs-xs); color: var(--text-2); line-height: var(--lh); }
</style>
