import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // No loro-crdt alias here: node_modules deps (loro-prosemirror) are externalized by vitest and
  // load loro's Node build, so our code must resolve to that same instance.
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    testTimeout: 15000,
    css: false,
  },
});
