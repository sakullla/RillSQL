<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { Listbox, Segmented, Switch, type ListboxOption, type SegmentedOption } from "../ui";
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

const sections = [
  { id: "appearance", label: "外观与编辑器" },
  { id: "reading", label: "数据读取" },
  { id: "local", label: "本地数据" },
  { id: "about", label: "关于" },
] as const;
type SectionId = (typeof sections)[number]["id"];

const activeSection = ref<SectionId>("appearance");
const pageEl = ref<HTMLElement | null>(null);

function goSection(id: SectionId) {
  activeSection.value = id;
  document.getElementById(id)?.scrollIntoView?.({ block: "start" });
}

// .page 是滚动容器（overflow: auto），滚动时取最后一个越过顶部阈值的分区。
function syncActiveSection() {
  const root = pageEl.value;
  if (!root) return;
  const rootTop = root.getBoundingClientRect().top;
  let current: SectionId = sections[0].id;
  for (const section of sections) {
    const el = document.getElementById(section.id);
    if (el && el.getBoundingClientRect().top - rootTop <= 72) current = section.id;
  }
  activeSection.value = current;
}

onMounted(() => pageEl.value?.addEventListener("scroll", syncActiveSection, { passive: true }));
onBeforeUnmount(() => pageEl.value?.removeEventListener("scroll", syncActiveSection));

const densityOptions: SegmentedOption[] = [
  { value: "compact", label: "紧凑" },
  { value: "standard", label: "标准" },
];
const editorFontOptions: ListboxOption[] = [12, 13, 14, 15, 16].map((size) => ({ value: String(size), label: `${size} px` }));
const pageSizeOptions: ListboxOption[] = [50, 100, 200, 500].map((size) => ({ value: String(size), label: `${size} 行` }));
const previewKBOptions: ListboxOption[] = [1, 4, 16].map((size) => ({ value: String(size), label: `${size} KB` }));
const objectPageOptions: ListboxOption[] = [100, 200, 500].map((size) => ({ value: String(size), label: `${size} 个` }));
const nullCopyOptions: ListboxOption[] = [
  { value: "NULL", label: "NULL" },
  { value: "empty", label: "空文本" },
  { value: "N", label: "\\N" },
];

function setTheme(theme: ThemePreference) {
  emit("change", { theme });
}
function setDensity(value: string) {
  if (value === "compact" || value === "standard") emit("change", { density: value });
}
function setNumber(key: "editorFont" | "pageSize" | "previewKB" | "objectPage", raw: string | null) {
  if (raw === null) return;
  const value = Number(raw);
  if (!Number.isFinite(value)) return;
  emit("change", { [key]: value });
}
function setNullCopy(raw: string | null) {
  if (raw === "NULL" || raw === "empty" || raw === "N") emit("change", { nullCopy: raw });
}
</script>

<template>
  <div ref="pageEl" class="page">
    <div class="page-inner narrow">
      <div class="page-head">
        <div>
          <h1>设置</h1>
          <p>修改立即生效，只保存在本机。</p>
        </div>
      </div>
      <div class="settings-layout">
        <nav class="settings-side" aria-label="设置分区">
          <button
            v-for="item in sections"
            :key="item.id"
            type="button"
            class="settings-side-link"
            :class="{ on: activeSection === item.id }"
            @click="goSection(item.id)"
          >{{ item.label }}</button>
        </nav>
        <div class="settings-main">
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
                <Segmented :model-value="settings.density" :options="densityOptions" aria-label="界面密度" @update:model-value="setDensity" />
              </div>
            </div>
            <div class="set-row">
              <div class="set-text">
                <h4>编辑器字号</h4>
                <p>同时影响 SQL 编辑器的行高。</p>
              </div>
              <div class="set-ctl">
                <Listbox
                  :model-value="String(settings.editorFont)"
                  :options="editorFontOptions"
                  aria-label="编辑器字号"
                  @update:model-value="setNumber('editorFont', $event)"
                />
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
                <Listbox
                  :model-value="String(settings.pageSize)"
                  :options="pageSizeOptions"
                  aria-label="每次读取的行数"
                  @update:model-value="setNumber('pageSize', $event)"
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-text">
                <h4>大字段预览上限</h4>
                <p>超过上限的值只显示预览，完整内容按需读取。</p>
              </div>
              <div class="set-ctl">
                <Listbox
                  :model-value="String(settings.previewKB)"
                  :options="previewKBOptions"
                  aria-label="大字段预览上限"
                  @update:model-value="setNumber('previewKB', $event)"
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-text">
                <h4>对象列表每次显示</h4>
                <p>对象很多时分批显示，避免一次加载全部。</p>
              </div>
              <div class="set-ctl">
                <Listbox
                  :model-value="String(settings.objectPage)"
                  :options="objectPageOptions"
                  aria-label="对象列表每次显示"
                  @update:model-value="setNumber('objectPage', $event)"
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-text">
                <h4>复制 NULL 时写入</h4>
                <p>“复制值”时 NULL 写成什么文本。“复制为 SQL 值”始终区分 NULL 与空字符串。</p>
              </div>
              <div class="set-ctl">
                <Listbox
                  :model-value="settings.nullCopy"
                  :options="nullCopyOptions"
                  aria-label="复制 NULL 时写入"
                  @update:model-value="setNullCopy"
                />
              </div>
            </div>
            <div class="set-row">
              <div class="set-text">
                <h4>复制选区时包含表头</h4>
                <p>Ctrl+C 多格复制时第一行是否写入列名。</p>
              </div>
              <div class="set-ctl">
                <Switch
                  :model-value="settings.tsvHeaders"
                  aria-label="复制选区时包含表头"
                  @update:model-value="emit('change', { tsvHeaders: $event })"
                />
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
          <section id="about" class="set-sec about">
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
    </div>
  </div>
</template>
