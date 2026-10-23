// Unit tests get the same build-time values as the site (vite.config.ts): the devnet cluster file and
// the program's IDL (target/idl/safu_pool.json, from `anchor idl build`; CI builds it in the program
// job and hands it to the site job). Without the IDL the tests refuse to start rather than skip.
import { readFileSync } from "node:fs";
import { defineConfig } from "vitest/config";

const readJson = (rel: string) => JSON.parse(readFileSync(new URL(rel, import.meta.url), "utf8"));

export default defineConfig({
  define: {
    __POOL__: JSON.stringify(readJson("../config/pool.devnet.json")),
    __IDL__: JSON.stringify(readJson("../target/idl/safu_pool.json")),
  },
  test: { include: ["src/**/*.test.ts"] },
});
