// Talks to backend/colosseum_app.py (the claim API). Message formats MUST stay byte-identical with it:
//   SAFU_SOLANA_COVERED_WALLET:v1:{staker}:{wallet}:{ts}
//   SAFU_SOLANA_CLAIM:v1:{staker}:{tx1,tx2,...}:{ts}
// The staking wallet signs each one (Solana signMessage). Nothing about the payout is typed in: the
// backend reads the loss, the hack time and the tier from the chain, and the oracle submits the claim.
import type { AppClient } from "./client";

export type CoveredWalletResponse = {
  staker: string;
  wallet: string;
  status: "registered" | "send_required" | "not_found_yet";
  registered_at?: number | null;
  method?: string | null;
  asset?: string | null;
  amount?: string | null;
  send_to?: string | null;
  expires_at?: number | null;
  proof_tx?: string | null;
  onchain?: string | null;
};

export type FileClaimResult = {
  claim_address: string;
  claim_status: string;
  tx_signature: string;
  staker: string;
  wallets: string[];
  entitlement_lamports: number;
  tier: string;
  demo_mode: Record<string, string>;
};

export class ClaimApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function postJson<T>(path: string, body: unknown): Promise<T> {
  const res = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const out = await res.json().catch(() => ({}));
  if (!res.ok) throw new ClaimApiError(res.status, typeof out.detail === "string" ? out.detail : `Request failed (${res.status})`);
  return out as T;
}

const now = () => Math.floor(Date.now() / 1000);

export async function addCoveredWallet(client: AppClient, staker: string, wallet: string): Promise<CoveredWalletResponse> {
  const timestamp = now();
  const signature = await client.signMessage(`SAFU_SOLANA_COVERED_WALLET:v1:${staker}:${wallet}:${timestamp}`);
  return postJson("/covered-wallets", { staker, wallet, timestamp, signature });
}

export function confirmCoveredWallet(staker: string, wallet: string): Promise<CoveredWalletResponse> {
  return postJson("/covered-wallets/confirm", { staker, wallet });
}

export async function fileClaim(client: AppClient, staker: string, txHashes: string[]): Promise<FileClaimResult> {
  const tx_hashes = txHashes.map((t) => t.trim());
  const timestamp = now();
  const signature = await client.signMessage(`SAFU_SOLANA_CLAIM:v1:${staker}:${tx_hashes.join(",")}:${timestamp}`);
  return postJson("/claim", { staker, tx_hashes, timestamp, signature });
}
