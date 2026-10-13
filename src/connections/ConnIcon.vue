<script setup lang="ts">
defineProps<{ name: string }>();

const paths: Record<string, string> = {
  database: "M4 6c0-1.7 3.6-3 8-3s8 1.3 8 3-3.6 3-8 3-8-1.3-8-3ZM4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3",
  search: "M4 11a7 7 0 1 0 14 0 7 7 0 1 0-14 0M20 20l-4-4",
  plus: "M12 5v14M5 12h14",
  file: "M6 2h8l5 5v15H6zM14 2v5h5",
  shield: "M12 3 5 6v6c0 4.2 2.8 7.4 7 9 4.2-1.6 7-4.8 7-9V6l-7-3Z",
  lock: "M5 11h14v10H5zM8 11V7a4 4 0 0 1 8 0v4",
  unlock: "M8 11V7a4 4 0 0 1 7.5-2M5 11h14v10H5z",
  tunnel: "M4 7h16M4 17h16M7 7v10M17 7v10",
  key: "M8 14a4 4 0 1 1 0-8 4 4 0 0 1 0 8Zm4-4h8M18 10v3",
  plug: "M9 7v4M15 7v4M8 11h8v2a4 4 0 0 1-8 0v-2ZM12 17v4",
  unplug: "M8 7v4M16 7v4M7 11h5M14 11h3M9 15l-2 4M15 15l2 4",
  more: "M6 12h.01M12 12h.01M18 12h.01",
  pencil: "M4 20h4l10-10-4-4L4 16v4ZM13 7l4 4",
  trash: "M4 7h16M9 7V5h6v2M8 7l1 13h6l1-13",
  play: "m8 6 10 6-10 6V6Z",
  info: "M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18ZM12 11v5M12 8h.01",
  check: "m5 12.5 4.5 4.5L19 7",
  x: "M6 6l12 12M18 6 6 18",
  minus: "M6 12h12",
  monitor: "M3 4h18v12H3zM8 20h8M12 16v4",
};
</script>

<template>
  <svg class="i" viewBox="0 0 24 24" aria-hidden="true">
    <path :d="paths[name] ?? ''" />
  </svg>
</template>
