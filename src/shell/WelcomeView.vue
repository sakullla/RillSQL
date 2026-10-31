<script setup lang="ts">
import { computed } from "vue";
import { catalogConnections } from "../catalog/store";
import { tr as t } from "../i18n";
import BrandMark from "./BrandMark.vue";
import Icon from "./Icon.vue";

const emit = defineEmits<{ "new-query": []; connections: []; sqlite: [] }>();
const saved = computed(() => catalogConnections().length > 0);
</script>

<template>
  <div class="welcome">
    <div class="welcome-inner">
      <template v-if="saved">
        <header class="desk-heading">
          <div>
            <h1>{{ t("shell.welcome.resumeTitle") }}</h1>
            <p>{{ t("shell.welcome.resumeHint") }}</p>
          </div>
          <button class="btn primary" type="button" :aria-label="t('common.newQuery')" @click="emit('new-query')"><Icon name="plus" />{{ t("common.newQuery") }}</button>
        </header>
        <!-- 继续态入口卡：与空态 .desk-action 同一质感；容器沿用 welcome-quick 锚点类，升级为卡片栅格。 -->
        <div class="welcome-quick">
          <button class="btn resume-action" type="button" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>{{ t("common.newConnection") }}</b><small>{{ t("shell.welcome.newConnectionHint") }}</small></span></button>
          <button class="btn resume-action" type="button" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>{{ t("common.openSqlite") }}</b><small>{{ t("shell.welcome.openSqliteHint") }}</small></span></button>
        </div>
      </template>
      <template v-else>
        <header class="welcome-hero">
          <BrandMark large />
          <div class="hero-copy">
            <p class="hero-kicker">{{ t("shell.welcome.kicker") }}</p>
            <h1>{{ t("shell.welcome.title") }}</h1>
            <p>{{ t("shell.welcome.subtitle") }}</p>
          </div>
        </header>
        <div class="desk-actions">
          <button class="btn primary desk-action" type="button" @click="emit('connections')"><span class="action-icon"><Icon name="database" /></span><span><b>{{ t("common.newConnection") }}</b><small>{{ t("shell.welcome.newConnectionHint") }}</small></span></button>
          <button class="btn desk-action" type="button" @click="emit('sqlite')"><span class="action-icon"><Icon name="file" /></span><span><b>{{ t("common.openSqlite") }}</b><small>{{ t("shell.welcome.openSqliteHint") }}</small></span></button>
        </div>
        <ul class="hero-facts">
          <li><Icon name="check" />{{ t("shell.welcome.factLocal") }}</li>
          <li><Icon name="check" />{{ t("shell.welcome.factKeyring") }}</li>
          <li><Icon name="check" />{{ t("shell.welcome.factEngines") }}</li>
        </ul>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 间距与字号尽量消费 --sp- 与 --fs- 前缀令牌；个别非 4px 刻度值（30px/28px/14px/10px）为既有视觉节奏，
   就近刻度会带来可感知的 2px 位移，按任务约定保留原值并在此说明。 */
/* .btn 的固定高度会裁切带说明的入口；实心仍只来自 .primary。 */
.desk-action.btn,
.resume-action.btn {
  height: auto;
  width: 100%;
  min-width: 0;
  justify-content: flex-start;
  white-space: normal;
  transition: background var(--dur-fast) var(--ease), border-color var(--dur-fast) var(--ease), color var(--dur-fast) var(--ease), box-shadow var(--dur-fast) var(--ease), transform var(--dur-fast) var(--ease);
}
.desk-action.primary .action-icon {
  background: color-mix(in srgb, var(--on-accent) 18%, transparent);
  color: var(--on-accent);
}
.desk-action.primary small {
  color: color-mix(in srgb, var(--on-accent) 78%, transparent);
}
/* 品牌区：BrandMark + 中文导语，双主题令牌取色，不出现硬编码颜色。 */
.welcome-hero { display: flex; align-items: center; gap: var(--sp-5); margin: var(--sp-2) 0 30px; flex-wrap: wrap; }
.hero-kicker { margin: 0 0 var(--sp-2); font: 600 var(--fs-xs)/1 var(--mono); letter-spacing: .12em; color: var(--accent-text); }
.welcome-hero h1 { font-size: var(--fs-xxl); letter-spacing: -.02em; margin: 0 0 10px; font-weight: 650; }
.welcome-hero p { margin: 0; color: var(--text-2); font-size: var(--fs-md); }
.hero-facts {
  list-style: none;
  display: flex;
  flex-wrap: wrap;
  gap: var(--sp-2) var(--sp-5);
  margin: 28px 0 0;
  padding: var(--sp-4) var(--sp-5);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  background: var(--surface-panel);
  box-shadow: var(--shadow-sm);
  color: var(--text-2);
  font-size: var(--fs-sm);
}
.hero-facts li { display: flex; align-items: center; gap: var(--sp-1); }
.hero-facts .i { width: 14px; height: 14px; color: var(--ok); flex-shrink: 0; }
/* 继续态入口卡：复用空态入口的图标语言与悬浮上移质感。 */
.welcome-quick { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; margin-bottom: var(--sp-7); }
.resume-action:hover { border-color: color-mix(in srgb, var(--accent) 45%, var(--line-2)); box-shadow: var(--shadow); transform: translateY(-1px); }
@media (max-width: 960px) {
  .welcome-quick { grid-template-columns: 1fr; gap: 10px; }
  .welcome-hero { align-items: flex-start; }
}
</style>
