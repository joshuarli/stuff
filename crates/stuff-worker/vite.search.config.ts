import { defineConfig } from "vite";

export default defineConfig({
  root: "../stuff-catalog",
  build: {
    target: "es2022",
    minify: true,
    lib: {
      entry: "web/entry.ts",
      formats: ["iife"],
      name: "packageSearch",
      fileName: () => "search.js",
    },
    outDir: "assets",
    emptyOutDir: false,
  },
});
