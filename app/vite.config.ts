import { readFileSync } from 'node:fs'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The pool switch, same as the backend's (backend/solana_pool.py): SAFU_SOLANA_CLUSTER=localnet|devnet
// picks config/pool.<cluster>.json. No mainnet file exists, so no mainnet build can be made. The program
// id, account layouts, seeds and every rule number come from the IDL, never from this app's code.
function cluster(mode: string): string {
  const c = process.env.SAFU_SOLANA_CLUSTER ?? (mode === 'test' ? 'devnet' : '')
  if (c !== 'localnet' && c !== 'devnet') throw new Error(`SAFU_SOLANA_CLUSTER=${JSON.stringify(c)}; set localnet or devnet`)
  return c
}

const readJson = (rel: string) => JSON.parse(readFileSync(new URL(rel, import.meta.url), 'utf8'))

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const c = cluster(mode)
  return {
    base: '/',
    plugins: [react()],
    define: {
      __POOL__: JSON.stringify(readJson(`../config/pool.${c}.json`)),
      __IDL__: JSON.stringify(readJson('../target/idl/safu_pool.json')),
    },
    server: {
      // backend/colosseum_app.py (claim API), run separately with uvicorn. SAFU_API moves it off :8000,
      // which solana-test-validator's own port range can take.
      proxy: Object.fromEntries(['/claim', '/covered-wallets'].map((p) => [p, process.env.SAFU_API ?? 'http://localhost:8000'])),
    },
  }
})
