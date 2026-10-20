import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";

// 外部环境可能把 NODE_ENV 设成 production；Vitest 会因此让 Vue 加载生产构建，
// 导致 @vue/test-utils 记录不到 emit。只归一测试进程，不碰生产构建。
if (process.env.VITEST) process.env.NODE_ENV = "test";

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_", "TAURI_"],
  build: {
    target: "es2022",
    sourcemap: false,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
  },
});
