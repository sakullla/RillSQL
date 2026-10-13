<script setup lang="ts">
import BrandMark from "./BrandMark.vue";
import ChordKeys from "./ChordKeys.vue";
import Icon from "./Icon.vue";

const emit = defineEmits<{
  "new-query": [];
  connections: [];
  sqlite: [];
  palette: [];
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
        <section>
          <h3>开始</h3>
          <button class="wl-item" type="button" @click="emit('new-query')">
            <Icon name="code" /><span>新建查询</span><ChordKeys chord="Mod+T" />
          </button>
          <button class="wl-item" type="button" @click="emit('connections')">
            <Icon name="plus" /><span>新建连接</span>
          </button>
          <button class="wl-item" type="button" @click="emit('sqlite')">
            <Icon name="file" /><span>打开 SQLite 文件</span>
          </button>
          <button class="wl-item" type="button" @click="emit('palette')">
            <Icon name="command" /><span>命令面板</span><ChordKeys chord="Mod+K" />
          </button>
        </section>
        <section>
          <h3>最近打开</h3>
          <p class="muted sm">还没有最近打开的表。连接数据库后，从资源管理器打开的表会出现在这里。</p>
        </section>
      </div>
    </div>
  </div>
</template>
