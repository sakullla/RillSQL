<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { riskLevelLabel, statusLabel, transactionSummary } from "./logic";
import type { StructurePlan, StructureReceipt } from "./types";

const props = defineProps<{ plan: StructurePlan; receipt: StructureReceipt | null; busy: boolean; expectName?: string; engine?: string }>();
const emit = defineEmits<{ cancel: []; confirm: [] }>();
const typed = ref("");
const copyNote = ref("");
const detailsOpen = ref(false);

const blocked = computed(() => props.plan.blocked.length > 0 || (props.receipt?.blocked.length ?? 0) > 0);
const nameOk = computed(() => !props.expectName || typed.value === props.expectName);
const receiptBad = computed(() => (props.receipt?.steps ?? []).some((step) => step.status === "failed" || step.status === "unknown"));
const steps = computed(() => props.receipt?.steps ?? props.plan.steps);
const txSummary = computed(() => transactionSummary(props.engine ?? "", props.plan.rebuild));

/* 失败场景自动展开与失败直接相关的部分（R3）。 */
watch(
  receiptBad,
  (bad) => {
    if (bad) detailsOpen.value = true;
  },
  { immediate: true },
);

async function copySql() {
  const sql = steps.value.map((step) => step.sql).join("\n");
  try {
    await navigator.clipboard.writeText(sql);
    copyNote.value = "已复制";
  } catch {
    copyNote.value = "复制失败";
  }
}
</script>

<template>
  <div class="overlay" role="presentation" @click.self="emit('cancel')">
    <div class="sheet" role="dialog" aria-modal="true" aria-label="变更预览">
      <header>
        <h2>{{ receipt ? "执行结果" : "执行前确认" }}</h2>
        <span class="grow"></span>
      </header>
      <div class="body">
        <p class="pv-target">目标：{{ plan.target }}</p>

        <section v-if="!receipt" class="pv-sec">
          <h3 class="pv-h">将要发生什么</h3>
          <ol v-if="plan.steps.length" class="pv-steps">
            <li v-for="(step, index) in plan.steps" :key="index">{{ step.label }}</li>
          </ol>
          <p v-for="(item, index) in plan.blocked" :key="`block-${index}`" class="struct-alert" role="alert">{{ item }}</p>
          <p v-if="!plan.steps.length && !plan.blocked.length" class="muted">没有需要执行的改动。</p>
        </section>

        <section v-if="plan.risks.length" class="pv-sec">
          <h3 class="pv-h">需要注意</h3>
          <p v-for="(risk, index) in plan.risks" :key="`risk-${index}`" class="pv-risk" :class="risk.level">
            <span class="badge" :class="risk.level === 'danger' ? 'err' : risk.level === 'warn' ? 'warn' : 'info'">{{ riskLevelLabel(risk.level) }}</span>
            <span>{{ risk.text }}</span>
          </p>
        </section>

        <p class="pv-tx"><span class="badge info">执行方式</span><span>{{ txSummary }}</span></p>

        <label v-if="expectName && !receipt" class="pv-confirm">
          这张表和它里面的数据会被永久删除，无法恢复。输入表名 {{ expectName }} 确认。
          <input v-model="typed" type="text" aria-label="确认表名" placeholder="输入表名" />
        </label>

        <p v-for="(item, index) in receipt?.blocked ?? []" :key="`done-${index}`" class="struct-alert" role="alert">{{ item }}</p>

        <section v-if="receipt" class="pv-sec">
          <h3 class="pv-h">每一步的结果</h3>
          <div v-for="(step, index) in receipt.steps" :key="`r-${index}`" class="receipt-row">
            <span
              class="badge"
              :class="step.status === 'ok' ? 'ok' : step.status === 'failed' || step.status === 'unknown' ? 'err' : 'warn'"
            >{{ statusLabel(step.status) }}</span>
            <span>{{ step.label }}<template v-if="step.message"> — {{ step.message }}</template></span>
          </div>
        </section>

        <details class="disclosure" :open="detailsOpen">
          <summary @click.prevent="detailsOpen = !detailsOpen">详情 <span class="muted">逐条 SQL 与完整说明</span></summary>
          <div v-if="detailsOpen" class="disclosure-body">
            <p class="muted">{{ receipt?.rule || plan.rule }}</p>
            <section v-for="(step, index) in steps" :key="`sql-${index}`" class="pv-sql">
              <strong>{{ index + 1 }}. {{ step.label }}</strong>
              <pre>{{ step.sql }}</pre>
            </section>
            <div>
              <button class="btn sm" type="button" @click="copySql">复制全部 SQL</button>
              <span v-if="copyNote" class="muted"> {{ copyNote }}</span>
            </div>
          </div>
        </details>
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn" type="button" @click="emit('cancel')">{{ receipt ? "关闭" : "取消" }}</button>
        <button v-if="!receipt" class="btn primary" type="button" :disabled="blocked || busy || !nameOk" @click="emit('confirm')">确认执行</button>
      </footer>
    </div>
  </div>
</template>
