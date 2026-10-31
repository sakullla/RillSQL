<script setup lang="ts">
import { computed } from "vue";
// 模板里的分支判定直接用 value.kind（保持模板类型收窄），computed 只承担空态判断。
import type { RedisValuePayload } from "../redis/api";

const props = defineProps<{ value: RedisValuePayload }>();

const empty = computed(() => {
  switch (props.value.kind) {
    case "string":
      return props.value.text.length === 0;
    case "hash":
      return props.value.entries.length === 0;
    case "list":
    case "set":
      return props.value.items.length === 0;
    case "zset":
      return props.value.entries.length === 0;
    case "stream":
      return props.value.messages.length === 0;
    default:
      return true;
  }
});
</script>

<!-- 结构化值查看器（R11）：按 Redis 类型渲染，不依赖 DataGrid。 -->
<template>
  <div class="redis-value" :data-value-kind="value.kind">
    <p v-if="value.kind === 'none'" class="rv-empty">键不存在或类型暂不支持结构化渲染。</p>
    <p v-else-if="empty" class="rv-empty">这个键没有内容（空集合/空字符串）。</p>
    <pre v-else-if="value.kind === 'string'" class="rv-text mono">{{ value.text }}</pre>
    <table v-else-if="value.kind === 'hash'" class="rv-table">
      <thead><tr><th>字段</th><th>值</th></tr></thead>
      <tbody>
        <tr v-for="entry in value.entries" :key="entry.field">
          <td class="mono rv-field">{{ entry.field }}</td>
          <td class="mono">{{ entry.value }}</td>
        </tr>
      </tbody>
    </table>
    <ol v-else-if="value.kind === 'list'" class="rv-list">
      <li v-for="(item, index) in value.items" :key="index" class="mono"><span class="rv-index">{{ index }}</span>{{ item }}</li>
    </ol>
    <ul v-else-if="value.kind === 'set'" class="rv-list">
      <li v-for="item in value.items" :key="item" class="mono">{{ item }}</li>
    </ul>
    <table v-else-if="value.kind === 'zset'" class="rv-table">
      <thead><tr><th>成员</th><th>分值</th></tr></thead>
      <tbody>
        <tr v-for="entry in value.entries" :key="entry.member">
          <td class="mono">{{ entry.member }}</td>
          <td class="mono rv-score">{{ entry.score }}</td>
        </tr>
      </tbody>
    </table>
    <div v-else-if="value.kind === 'stream'" class="rv-stream">
      <article v-for="message in value.messages" :key="message.id" class="rv-message">
        <header class="mono">{{ message.id }}</header>
        <dl>
          <template v-for="field in message.fields" :key="field.field">
            <dt class="mono">{{ field.field }}</dt>
            <dd class="mono">{{ field.value }}</dd>
          </template>
        </dl>
      </article>
    </div>
  </div>
</template>

<style scoped>
.redis-value { min-height: 0; overflow: auto; }
.rv-empty { color: var(--text-3); padding: var(--sp-2) 0; }
.rv-text { white-space: pre-wrap; word-break: break-all; margin: 0; padding: var(--sp-2); border: 1px solid var(--line); border-radius: var(--r-sm, 6px); }
.rv-table { border-collapse: collapse; width: 100%; }
.rv-table th { text-align: left; font-size: 12px; color: var(--text-3); padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); position: sticky; top: 0; background: var(--bg-1, var(--bg)); }
.rv-table td { padding: var(--sp-1) var(--sp-2); border-bottom: 1px solid var(--line); vertical-align: top; word-break: break-all; }
.rv-field { color: var(--text-2); }
.rv-score { text-align: right; white-space: nowrap; }
.rv-list { margin: 0; padding: 0; list-style: none; display: grid; gap: 2px; }
.rv-list li { padding: var(--sp-1) var(--sp-2); border-bottom: 1px dashed var(--line); word-break: break-all; }
.rv-index { color: var(--text-3); margin-right: var(--sp-2); font-size: 12px; }
.rv-stream { display: grid; gap: var(--sp-2); }
.rv-message { border: 1px solid var(--line); border-radius: var(--r-sm, 6px); padding: var(--sp-2); display: grid; gap: var(--sp-1); }
.rv-message header { color: var(--text-3); font-size: 12px; }
.rv-message dl { margin: 0; display: grid; grid-template-columns: minmax(90px, auto) 1fr; gap: 2px var(--sp-2); }
.rv-message dt { color: var(--text-3); }
.rv-message dd { margin: 0; word-break: break-all; }
.mono { font-family: var(--mono, ui-monospace, monospace); }
</style>
