import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // Dev only: point at the running engine (start.sh prints its port; the desktop app picks 8766–8785).
  server: { proxy: { "/api": `http://127.0.0.1:${process.env.VOICEFORGE_PORT ?? "8766"}` } },
});
