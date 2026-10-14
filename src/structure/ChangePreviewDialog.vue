<script setup lang="ts">
import { ref } from "vue";
import { statusLabel } from "./logic";
import type { StructurePlan, StructureReceipt } from "./types";

const props = defineProps<{ plan: StructurePlan; receipt: StructureReceipt | null; busy: boolean; expectName?: string }>();
const emit = defineEmits<{ cancel: []; confirm: [] }>();
const typed = ref("");
const copyNote = ref("");

const blocked = () => props.plan.blocked.length > 0 || (props.receipt?.blocked.length ?? 0) > 0;
const nameOk = () => !props.expectName || typed.value === props.expectName;

async function copySql() {
  const sql = (props.receipt?.steps ?? props.plan.steps).map((step) => step.sql).join("\n");
  try {
    await navigator.clipboard.writeText(sql);
    copyNote.value = "已复制";
  } catch {
    copyNote.value = "复制失败";
  }
}
</script>

<template>
  <div class="structure-mask" role="presentation" @click.self="emit('cancel')">
    <div class="structure-sheet" role="dialog" aria-label="变更预览">
      <header>
        <h2>变更预览</h2>
        <span class="grow"></span>
        <button class="btn" type="button" @click="copySql">复制 SQL</button>
      </header>
      <div class="body">
        <p>目标：{{ plan.target }}</p>
        <p>{{ receipt?.rule || plan.rule }}</p>
        <p v-for="(risk, index) in plan.risks" :key="index" class="risk" :class="risk.level">{{ risk.text }}</p>
        <p v-for="(item, index) in plan.blocked" :key="`block-${index}`" class="struct-alert" role="alert">{{ item }}</p>
        <p v-for="(item, index) in receipt?.blocked ?? []" :key="`done-${index}`" class="struct-alert" role="alert">{{ item }}</p>
        <section v-for="(step, index) in plan.steps" :key="`${index}-${step.label}`">
          <strong>{{ index + 1 }}. {{ step.label }}</strong>
          <pre>{{ step.sql }}</pre>
        </section>
        <label v-if="expectName && !receipt">输入表名 {{ expectName }} 以确认删除
          <input v-model="typed" type="text" aria-label="确认表名" />
        </label>
        <div v-if="receipt">
          <h3>执行结果</h3>
          <div v-for="(step, index) in receipt.steps" :key="`r-${index}`" class="receipt-row">
            <b :class="step.status">{{ statusLabel(step.status) }}</b>
            <span>{{ step.label }}<template v-if="step.message"> — {{ step.message }}</template></span>
          </div>
        </div>
        <p v-if="copyNote" class="muted">{{ copyNote }}</p>
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('cancel')">{{ receipt ? "关闭" : "取消" }}</button>
        <button v-if="!receipt" class="btn primary" type="button" :disabled="blocked() || busy || !nameOk()" @click="emit('confirm')">执行</button>
      </footer>
    </div>
  </div>
</template>
