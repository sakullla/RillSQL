<script setup lang="ts">
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";

const emit = defineEmits<{
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
  structure: [];
}>();
</script>

<template>
  <div class="welcome">
    <div class="welcome-inner">
      <div class="welcome-brand">
        <BrandMark large />
        <div>
          <h1>澄库 <span>RillSQL</span></h1>
          <p>清晰、轻量的 SQL 工作台。所有数据留在本机，无需账号。</p>
        </div>
      </div>
      <div class="welcome-grid">
        <button class="welcome-path" type="button" @click="emit('connections')">
          <Icon name="plus" />
          <span class="wp-title">新建连接</span>
          <span class="wp-desc">连接 MySQL、PostgreSQL 或 MariaDB，填几项就能开始。</span>
        </button>
        <button class="welcome-path" type="button" @click="emit('sqlite')">
          <Icon name="file" />
          <span class="wp-title">打开 SQLite 文件</span>
          <span class="wp-desc">直接打开本机上的一个数据库文件，不用配置服务器。</span>
        </button>
      </div>
      <div class="welcome-more">
        <span class="muted sm">已经连好了？</span>
        <button class="btn ghost sm" type="button" @click="emit('new-query')">
          <Icon name="code" />新建查询<ChordKeys chord="Mod+T" />
        </button>
        <button class="btn ghost sm" type="button" @click="emit('structure')">
          <Icon name="table" />新建表结构
        </button>
        <button class="btn ghost sm" type="button" @click="emit('palette')">
          <Icon name="command" />命令面板<ChordKeys chord="Mod+K" />
        </button>
      </div>
    </div>
  </div>
</template>
