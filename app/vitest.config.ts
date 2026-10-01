// Unit tests run without the program's IDL: vite.config.ts reads target/idl/safu_pool.json, which only
// exists after an Anchor build. Tests that need the IDL belong next to the program, not here.
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: { include: ["src/**/*.test.ts"] },
});
