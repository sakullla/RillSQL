<script setup lang="ts">
import { computed } from "vue";
import { catalogConnections } from "../catalog/store";
import { engineMeta } from "../connections/model";
import { CONNECTION_STATUS_TEXT } from "./status-text";
import type { QueryTarget } from "./session";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";
const emit = defineEmits<{
  "new-query": []; connections: []; sqlite: []; palette: []; structure: [];
  "query-target": [target: QueryTarget];
}>();
const connections = computed(() => catalogConnections());
const connected = computed(() => connections.value.filter(item => item.status === "connected").length);
function openConnection(id: string) {
  const item = connections.value.find(item => item.id === id);
  if (!item || item.status !== "connected") { emit("connections"); return; }
  const database = item.defaultDatabase ?? item.databases[0]?.name;
  if (database == null) { emit("connections"); return; }
  emit("query-target", { connectionId: item.id, connectionName: item.name, database, engine: item.engine, status: item.status });
}
</script>
<template>
  <div class="welcome">
    <div class="welcome-inner">
      <header class="desk-heading">
        <div><div class="eyebrow">WORKSPACE <span>/</span> 本地工作空间</div><h1>从数据，到答案。</h1><p>浏览数据库，编写 SQL，让每一次查询都有清晰的上下文。</p></div>
        <button class="btn primary" type="button" aria-label="新建查询" @click="emit('new-query')"><Icon name="plus" />新建查询 <ChordKeys chord="Mod+T" /></button>
      </header>
      <div class="desk-actions">
        <button class="desk-action" type="button" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>新建连接</b><small>PostgreSQL / MySQL / MariaDB</small></span><span class="action-arrow">↗</span></button>
        <button class="desk-action" type="button" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>打开 SQLite 文件</b><small>直接进入本地数据库</small></span><span class="action-arrow">↗</span></button>
      </div>
      <div class="desk-columns">
        <section class="desk-connections">
          <div class="section-heading"><h2>你的数据库 <span class="count">{{ connections.length }}</span></h2><span class="muted sm">{{ connected }} 个已连接</span></div>
          <div v-if="!connections.length" class="desk-empty"><Icon name="database" /><h3>添加第一个数据库</h3><p>连接后，表、视图和结构会出现在左侧。<br />所有连接配置保存在本机，无需账号。</p><button class="btn" @click="emit('connections')">配置连接</button></div>
          <div v-else class="desk-db-list">
            <button v-for="item in connections" :key="item.id" class="desk-db" @click="openConnection(item.id)">
              <span class="engine-tile" :class="`engine-${item.engine}`">{{ engineMeta(item.engine).short }}</span>
              <span class="db-copy"><b>{{ item.name }}</b><small>{{ engineMeta(item.engine).label }} · {{ item.defaultDatabase || item.databases[0]?.name || '未选择数据库' }}</small></span>
              <span class="env" :class="`env-${item.envTone}`">{{ item.envLabel || '无标签' }}</span>
              <span class="conn-state" :class="`st-${item.status}`"><i></i>{{ CONNECTION_STATUS_TEXT[item.status] }}</span><span class="action-arrow">→</span>
            </button>
          </div>
          <button class="desk-manage" @click="emit('connections')">管理全部连接 <span>→</span></button>
        </section>
        <aside class="desk-tools">
          <div class="section-heading"><h2>更快开始</h2><Icon name="terminal" /></div>
          <button @click="emit('palette')"><span><Icon name="search" />查找表与命令</span><ChordKeys chord="Mod+K" /></button>
          <button @click="emit('new-query')"><span><Icon name="code" />打开 SQL 编辑器</span><ChordKeys chord="Mod+T" /></button>
          <button @click="emit('structure')"><span><Icon name="table" />设计新表结构</span><span>↗</span></button>
          <div class="desk-tip"><span class="eyebrow">SQL FIRST</span><h3>让键盘跟上你的思路。</h3><p>在编辑器中选中 SQL，按 <ChordKeys chord="Mod+Enter" /> 执行选中部分。执行前，随时确认连接与环境。</p></div>
        </aside>
      </div>
      <footer class="desk-footer"><span><Icon name="lock" />本地配置 · 系统凭据库保管密码</span><span class="mono">RillSQL / 澄库</span></footer>
    </div>
  </div>
</template>
