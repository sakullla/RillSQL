<script setup lang="ts">
import { computed } from "vue";
import { ABOUT_ABSENT, ABOUT_FACTS, ABOUT_TAGLINE, PRODUCT_LICENSE, PRODUCT_TITLE, PRODUCT_VERSION } from "./product";
import { localDataPathLabel, type ShellSettings } from "./settings";
import { detectMac } from "./shortcuts";
import BrandMark from "./BrandMark.vue";
import Icon from "./Icon.vue";
import type { Density, ThemePreference } from "./theme";

const props = defineProps<{ settings: ShellSettings; dataDir: string | null }>();
const emit = defineEmits<{ change: [patch: Partial<ShellSettings>]; help: [] }>();

const mac = detectMac();
const linux = !mac && /Linux/i.test(globalThis.navigator?.userAgent ?? "");
const pathLabel = computed(() => props.dataDir || localDataPathLabel(mac, linux));

const themes: { id: ThemePreference; label: string; icon: "sun" | "moon" | "monitor" }[] = [
  { id: "light", label: "浅色", icon: "sun" },
  { id: "dark", label: "深色", icon: "moon" },
  { id: "system", label: "跟随系统", icon: "monitor" },
];

function scrollSection(id: string) { document.getElementById(id)?.scrollIntoView({ block: "start" }); }

function setTheme(theme: ThemePreference) {
  emit("change", { theme });
}
function setDensity(density: Density) {
  emit("change", { density });
}
function onNumber(key: "editorFont" | "pageSize" | "previewKB" | "objectPage", event: Event) {
  emit("change", { [key]: Number((event.target as HTMLSelectElement).value) });
}
function onNullCopy(event: Event) {
  const value = (event.target as HTMLSelectElement).value;
  if (value === "NULL" || value === "empty" || value === "N") emit("change", { nullCopy: value });
}
</script>

<template>
  <div class="page">
    <div class="page-inner narrow">
      <div class="page-head">
        <div>
          <h1>设置</h1>
          <p>修改立即生效，只保存在本机。</p>
        </div>
      </div>
      <nav class="settings-nav" aria-label="设置分区"><button class="btn sm" @click="scrollSection('appearance')">外观与编辑器</button><button class="btn sm" @click="scrollSection('reading')">数据读取</button><button class="btn sm" @click="scrollSection('local')">本地数据</button></nav>
      <section id="appearance" class="set-sec">
        <h2>外观</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>主题</h4>
            <p>编辑器、表格和对话框同步切换。颜色从不作为唯一的信息来源。</p>
          </div>
          <div class="set-ctl">
            <div class="theme-cards">
              <label v-for="item in themes" :key="item.id" class="theme-card" :class="{ on: settings.theme === item.id }">
                <input type="radio" name="set-theme" :value="item.id" :checked="settings.theme === item.id" @change="setTheme(item.id)" />
                <span class="tc-prev" :class="`tc-${item.id}`"><i></i><i></i><i></i></span>
                <span><Icon :name="item.icon" />{{ item.label }}</span>
              </label>
            </div>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>界面密度</h4>
            <p>紧凑适合大屏阅读更多行；标准更易点按。</p>
          </div>
          <div class="set-ctl">
            <div class="seg-radio">
              <label :class="{ on: settings.density === 'compact' }">
                <input type="radio" name="set-density" value="compact" :checked="settings.density === 'compact'" @change="setDensity('compact')" />紧凑
              </label>
              <label :class="{ on: settings.density === 'standard' }">
                <input type="radio" name="set-density" value="standard" :checked="settings.density === 'standard'" @change="setDensity('standard')" />标准
              </label>
            </div>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>编辑器字号</h4>
            <p>同时影响 SQL 编辑器的行高。</p>
          </div>
          <div class="set-ctl">
            <select :value="settings.editorFont" aria-label="编辑器字号" @change="onNumber('editorFont', $event)">
              <option v-for="size in [12, 13, 14, 15, 16]" :key="size" :value="size">{{ size }} px</option>
            </select>
          </div>
        </div>
      </section>
      <section id="reading" class="set-sec">
        <h2>数据读取</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>每次读取的行数</h4>
            <p>表格和查询结果首次读取的上限；更多行需要主动继续读取。</p>
          </div>
          <div class="set-ctl">
            <select :value="settings.pageSize" aria-label="每次读取的行数" @change="onNumber('pageSize', $event)">
              <option v-for="size in [50, 100, 200, 500]" :key="size" :value="size">{{ size }} 行</option>
            </select>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>大字段预览上限</h4>
            <p>超过上限的值只显示预览，完整内容按需读取。</p>
          </div>
          <div class="set-ctl">
            <select :value="settings.previewKB" aria-label="大字段预览上限" @change="onNumber('previewKB', $event)">
              <option v-for="size in [1, 4, 16]" :key="size" :value="size">{{ size }} KB</option>
            </select>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>对象列表每次显示</h4>
            <p>对象很多时分批显示，避免一次加载全部。</p>
          </div>
          <div class="set-ctl">
            <select :value="settings.objectPage" aria-label="对象列表每次显示" @change="onNumber('objectPage', $event)">
              <option v-for="size in [100, 200, 500]" :key="size" :value="size">{{ size }} 个</option>
            </select>
          </div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>复制 NULL 时写入</h4>
            <p>“复制值”时 NULL 写成什么文本。“复制为 SQL 值”始终区分 NULL 与空字符串。</p>
          </div>
          <div class="set-ctl">
            <select :value="settings.nullCopy" aria-label="复制 NULL 时写入" @change="onNullCopy">
              <option value="NULL">NULL</option>
              <option value="empty">空文本</option>
              <option value="N">\N</option>
            </select>
          </div>
        </div>
      </section>
      <section id="local" class="set-sec">
        <h2>本地数据</h2>
        <div class="set-row">
          <div class="set-text">
            <h4>配置与历史的位置</h4>
            <p>连接配置、查询历史和已信任的 SSH 主机密钥保存在本机的一个 SQLite 文件中；密码不在其中。</p>
          </div>
          <div class="set-ctl"><code class="path">{{ pathLabel }}</code></div>
        </div>
        <div class="set-row">
          <div class="set-text">
            <h4>键盘快捷键</h4>
            <p>全部快捷键都可以在界面中找到。</p>
          </div>
          <div class="set-ctl">
            <button class="btn" type="button" @click="emit('help')"><Icon name="keyboard" />查看快捷键</button>
          </div>
        </div>
      </section>
      <section class="set-sec about">
        <div class="about-card">
          <BrandMark large />
          <div>
            <h3>{{ PRODUCT_TITLE }}</h3>
            <p>版本 {{ PRODUCT_VERSION }} · {{ PRODUCT_LICENSE }}</p>
            <p class="muted sm">{{ ABOUT_TAGLINE }}</p>
            <p class="about-absent">{{ ABOUT_ABSENT }}</p>
            <ul class="about-facts">
              <li v-for="fact in ABOUT_FACTS" :key="fact"><Icon name="check" />{{ fact }}</li>
            </ul>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>
