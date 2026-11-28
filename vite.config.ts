import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(({ mode }) => ({
  clearScreen: false,
  build: mode === "swift-manager" ? {
    outDir: "swift-host/Assets",
    modulePreload: false,
    rollupOptions: { output: { format: "iife", inlineDynamicImports: true } },
  } : undefined,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
