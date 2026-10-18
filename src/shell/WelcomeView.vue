<script setup lang="ts">
import { computed } from "vue";
import { catalogConnections } from "../catalog/store";
import Icon from "./Icon.vue";

const emit = defineEmits<{ "new-query": []; connections: []; sqlite: [] }>();
const saved = computed(() => catalogConnections().length > 0);
</script>

<template>
  <div class="welcome">
    <div class="welcome-inner">
      <header v-if="saved" class="desk-heading">
        <div>
          <h1>继续工作</h1>
          <p>已保存的连接在左侧对象树里，这里不再重复列出。</p>
        </div>
        <button class="btn primary" type="button" aria-label="新建查询" @click="emit('new-query')"><Icon name="plus" />新建查询</button>
      </header>
      <template v-else>
        <header class="desk-heading compact">
          <div>
            <h1>澄库 · RillSQL</h1>
            <p>配置留在本机，密码进入系统凭据库，无需账号。</p>
          </div>
        </header>
        <ul class="desk-entries">
          <li>
            <button class="desk-entry" type="button" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>新建连接</b><small>PostgreSQL / MySQL / MariaDB</small></span><span class="action-arrow" aria-hidden="true">›</span></button>
          </li>
          <li>
            <button class="desk-entry" type="button" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>打开 SQLite 文件</b><small>选择本机上的数据库文件</small></span><span class="action-arrow" aria-hidden="true">›</span></button>
          </li>
        </ul>
      </template>
    </div>
  </div>
</template>
