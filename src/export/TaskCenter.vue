<script setup lang="ts">
import { computed, ref } from "vue";
import { cancelRemoteExport, clearFinishedTasks, exportTasks } from "./tasks";

const open = ref(false);
const running = computed(() => exportTasks.find((task) => task.status === "running"));
const label = computed(() => (running.value ? `导出 ${running.value.doneRows}/${running.value.totalRows}` : "任务"));

function cancel(id: string) {
  void cancelRemoteExport(id);
}

function statusText(status: string, partial: boolean, done: number, total: number, message: string): string {
  if (status === "running") return `${done} / ${total} 行`;
  if (status === "done") return `已完成 · 写入 ${total} 行${partial ? " · 部分结果" : ""}`;
  if (status === "cancelled") return "已取消 · 未完成的文件已删除";
  if (status === "failed") return message;
  return message;
}
</script>

<template>
  <div class="task-center" data-module="export">
    <button class="tb-task" :class="{ busy: running }" type="button" aria-label="后台任务" :aria-expanded="open" @click="open = !open">
      {{ label }}
    </button>
    <div v-if="open" class="tasks-pop" role="dialog" aria-label="后台任务">
      <div class="tasks-head">
        <strong>后台任务</strong>
        <button class="btn" type="button" @click="clearFinishedTasks">清除已结束</button>
      </div>
      <p v-if="!exportTasks.length" class="muted">没有后台任务</p>
      <article v-for="task in exportTasks" :key="task.id" class="task-row" :data-status="task.status">
        <div class="task-top">
          <span class="task-name" :title="task.path">{{ task.fileName }}</span>
          <button v-if="task.status === 'running'" class="btn" type="button" @click="cancel(task.id)">取消</button>
        </div>
        <div class="progress" aria-hidden="true"><i :style="{ width: `${Math.floor((task.doneRows / Math.max(1, task.totalRows)) * 100)}%` }"></i></div>
        <small>{{ statusText(task.status, task.partial, task.doneRows, task.totalRows, task.message) }}</small>
      </article>
    </div>
  </div>
</template>

<style scoped>
.task-center { position: relative; }
.tb-task { height: 26px; padding: 0 10px; border-radius: 999px; background: var(--sunken); color: var(--text-2); font-size: 12px; }
.tb-task.busy { background: var(--accent-soft); color: var(--accent-text); }
.tasks-pop { position: absolute; right: 0; top: 32px; width: 320px; background: var(--panel); border: 1px solid var(--line-2); border-radius: 10px; box-shadow: var(--shadow); padding: 8px; z-index: 30; }
.tasks-head { display: flex; justify-content: space-between; align-items: center; gap: 8px; margin-bottom: 6px; }
.muted { color: var(--text-3); font-size: 12px; }
.task-row { padding: 6px 0; border-bottom: 1px solid var(--line); }
.task-name { font-size: 12.5px; }
.task-row small { display: block; color: var(--text-2); }
.progress { height: 4px; background: var(--sunken); border-radius: 99px; margin: 4px 0; }
.progress i { display: block; height: 100%; background: var(--accent); }
</style>
