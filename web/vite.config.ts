import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// In dev, the viewer runs on Vite and proxies signaling to the local rd-server.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/ws": { target: "ws://localhost:8080", ws: true },
      "/healthz": "http://localhost:8080",
    },
  },
});
