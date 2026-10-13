<script setup lang="ts">
import { ref, watch } from "vue";
import { pickSqliteFile } from "./index";

const props = defineProps<{ sqlitePrompt: number }>();
const message = ref("连接管理占位。保存、测试、断开和删除配置，以及 TLS 与 SSH，由连接模块实现。删除配置不会删除数据库文件。");
const busy = ref(false);

async function chooseFile() {
  busy.value = true;
  try {
    const file = await pickSqliteFile();
    message.value = file ? `已选择文件 ${file}。后续连接模块才会打开它。` : "没有选择文件。";
  } catch {
    message.value = "文件对话框只在桌面窗口里可用。不需要云登录也可以留在这里。";
  } finally {
    busy.value = false;
  }
}

watch(
  () => props.sqlitePrompt,
  (value) => {
    if (value > 0) void chooseFile();
  },
);
</script>

<template>
  <div class="page">
    <div class="page-inner">
      <div class="page-head">
        <div>
          <h1>连接</h1>
          <p>{{ message }}</p>
        </div>
        <div class="empty-actions">
          <button class="btn" type="button" disabled>新建连接</button>
          <button class="btn primary" type="button" :disabled="busy" @click="chooseFile">打开 SQLite 文件</button>
        </div>
      </div>
    </div>
  </div>
</template>
