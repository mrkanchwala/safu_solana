// WalletConnect for Solana (phone wallets), from the multichain site's lib/walletconnect.ts with the
// Ethereum half removed. SignClient owns the session; AppKit is only the QR / wallet-list modal
// (`manualWCControl`). Loaded with dynamic import() on first use, so its ~2 MB never ships to visitors
// who don't pick WalletConnect.
//
// The project id is safustaking.com's and is domain-allowlisted: solana.safustaking.com must be on its
// allowlist before this works on the live site.
//
// Signing only: the app never asks the wallet for reads.

import { SignClient } from "@walletconnect/sign-client";
import type { SessionTypes } from "@walletconnect/types";
import { createAppKit } from "@reown/appkit/core";
import { solanaDevnet } from "@reown/appkit/networks";
import {
  getBase58Decoder,
  getBase58Encoder,
  getBase64Decoder,
  getBase64Encoder,
  getTransactionDecoder,
  getTransactionEncoder,
} from "@solana/kit";
import type { Address, SignatureBytes } from "@solana/kit";

export const WALLETCONNECT_PROJECT_ID = "3824772bb6c01d55a924dac308a3cb3e";

const SOL_CHAIN = solanaDevnet.caipNetworkId;
// Mainnet ids are ASKED FOR TOO: a proposal listing only a test
// cluster is rejected by most phone wallets. Nothing is ever sent on mainnet: Solana signatures don't
// depend on the cluster (the devnet blockhash is inside the signed bytes), so a wallet that only
// approves mainnet can still sign our devnet transactions.
const SOL_MAINNETS = ["solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp", "solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ"];
const METADATA = {
  name: "SAFU on Solana",
  description: "Stake SOL into the SAFU pool and get your wallets covered.",
  url: "https://solana.safustaking.com",
  icons: ["https://safustaking.com/favicon.svg"],
};

export type WcSession = { address: string; topic: string; chainId: string };

let client: Awaited<ReturnType<typeof SignClient.init>> | null = null;
let modal: ReturnType<typeof createAppKit> | null = null;

async function setup() {
  if (!client) client = await SignClient.init({ projectId: WALLETCONNECT_PROJECT_ID, metadata: METADATA, customStoragePrefix: "safu-solana" });
  if (!modal) {
    modal = createAppKit({
      projectId: WALLETCONNECT_PROJECT_ID,
      manualWCControl: true,
      networks: [solanaDevnet],
      metadata: METADATA,
      features: { analytics: false, email: false, socials: false, swaps: false, onramp: false },
    });
  }
  return { client, modal };
}

const CHAINS = [...new Set([SOL_CHAIN, ...SOL_MAINNETS])];

/** Pool cluster if approved, else mainnet (same key, cluster-independent signatures). CAIP-10: `ns:ref:address`. */
function accountFor(session: SessionTypes.Struct): { address: string; chainId: string } | null {
  const accounts = Object.values(session.namespaces).flatMap((ns) => ns.accounts);
  for (const chain of CHAINS) {
    const hit = accounts.find((a) => a.startsWith(chain + ":"));
    if (hit) return { address: hit.slice(chain.length + 1), chainId: chain };
  }
  return null;
}

/** Opens the QR modal, waits for the phone wallet to approve. Rejects if the user closes it. */
export async function connect(): Promise<WcSession> {
  const { client: c, modal: m } = await setup();
  const { uri, approval } = await c.connect({
    optionalNamespaces: { solana: { methods: ["solana_signTransaction", "solana_signMessage"], chains: CHAINS, events: [] } },
  });
  if (uri) await m.open({ uri });

  // Closing the modal never rejects `approval()`, so race it against the modal closing.
  let unsubscribe = () => {};
  const closed = new Promise<never>((_, reject) => {
    unsubscribe = m.subscribeState((s) => {
      if (!s.open) reject({ code: -1, message: "The user closed the modal." });
    });
  });
  try {
    const session = await Promise.race([approval(), closed]);
    const hit = accountFor(session);
    if (!hit) {
      void c.disconnect({ topic: session.topic, reason: { code: 6000, message: "Wrong network" } });
      throw new Error(`Wrong network -- ${session.peer?.metadata?.name || "Your wallet"} didn't share a Solana account.`);
    }
    return { address: hit.address, topic: session.topic, chainId: hit.chainId };
  } finally {
    unsubscribe();
    void m.close();
  }
}

export async function disconnect(s: WcSession): Promise<void> {
  if (!client) return;
  await client.disconnect({ topic: s.topic, reason: { code: 6000, message: "User disconnected" } }).catch(() => {});
}

async function request<T>(s: WcSession, method: string, params: unknown): Promise<T> {
  const { client: c } = await setup();
  return c.request<T>({ topic: s.topic, chainId: s.chainId, request: { method, params } });
}

/** Sign a wire-format transaction, return the signed wire bytes. Wallets answer either with the whole
 *  signed transaction (base64) or with just our signature (base58, older spec). */
export async function solanaSignTransaction(s: WcSession, tx: Uint8Array): Promise<Uint8Array> {
  const out = await request<{ transaction?: string; signature?: string }>(s, "solana_signTransaction", {
    transaction: getBase64Decoder().decode(tx),
    pubkey: s.address,
  });
  if (out.transaction) return new Uint8Array(getBase64Encoder().encode(out.transaction));
  if (!out.signature) throw new Error("The wallet returned no signature.");
  const decoded = getTransactionDecoder().decode(tx);
  const signed = {
    ...decoded,
    signatures: { ...decoded.signatures, [s.address as Address]: new Uint8Array(getBase58Encoder().encode(out.signature)) as SignatureBytes },
  };
  return new Uint8Array(getTransactionEncoder().encode(signed));
}

/** Sign raw message bytes. Returns the signature as hex, no 0x (same shape as the extension path). */
export async function signMessage(s: WcSession, bytes: Uint8Array): Promise<string> {
  const out = await request<{ signature: string }>(s, "solana_signMessage", {
    message: getBase58Decoder().decode(bytes),
    pubkey: s.address,
  });
  return Array.from(getBase58Encoder().encode(out.signature), (x) => x.toString(16).padStart(2, "0")).join("");
}
