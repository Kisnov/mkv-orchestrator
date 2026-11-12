import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    proxy: {
      "/api": "http://localhost:8080"
    }
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("/src/generated/contracts.ts")) {
            return "contracts";
          }
          // React changes far less often than the rest, so it caches apart.
          if (/\/node_modules\/(react|react-dom|scheduler)\//.test(id)) {
            return "react";
          }
          if (id.includes("/node_modules/")) {
            return "vendor";
          }
        }
      }
    }
  }
});
