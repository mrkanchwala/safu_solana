// The pool's cluster file (config/pool.<cluster>.json) and the program's IDL, both baked in at build
// time (vite.config.ts). Read network values and rule numbers from here, never hard-code them.
declare const __POOL__: PoolConfig;
declare const __IDL__: Idl;

export interface PoolConfig {
  cluster: "localnet" | "devnet";
  rpcUrl: string;
  /** Same-origin chain-read path on the deployed site (the claim API relays it); unset = rpcUrl. */
  siteRpcPath?: string;
  poolCapLamports: number;
  computeUnitLimit: number;
  marinade: { program: string; state: string; msolMint: string };
}

export type IdlType =
  | string
  | { array: [IdlType, number] }
  | { option: IdlType }
  | { vec: IdlType }
  | { defined: { name: string } };
export type IdlField = { name: string; type: IdlType };
export type IdlSeed = { kind: "const"; value: number[] } | { kind: "account" | "arg"; path: string };
export type IdlAccountItem = {
  name: string;
  writable?: boolean;
  signer?: boolean;
  address?: string;
  pda?: { seeds: IdlSeed[]; program?: unknown };
  accounts?: IdlAccountItem[];
};
export interface Idl {
  address: string;
  constants: { name: string; type: string; value: string }[];
  instructions: { name: string; discriminator: number[]; args: IdlField[]; accounts: IdlAccountItem[] }[];
  accounts: { name: string; discriminator: number[] }[];
  types: { name: string; type: { kind: "struct"; fields: IdlField[] } | { kind: "enum"; variants: { name: string }[] } }[];
  errors?: { code: number; name: string; msg?: string }[];
}

export const POOL: PoolConfig = __POOL__;
export const IDL: Idl = __IDL__;
export const PROGRAM_ID = IDL.address;
export const IS_LOCALNET = POOL.cluster === "localnet";

/** The wallet-standard chain the wallet signs for. */
export const WALLET_CHAIN = `solana:${POOL.cluster}` as const;

function rawConstant(name: string): { type: string; value: string } {
  const c = IDL.constants.find((x) => x.name === name);
  if (!c) throw new Error(`IDL has no constant ${name}`);
  return c;
}

/** An integer rule number from the IDL (u8..u128, i64). */
export function num(name: string): bigint {
  return BigInt(rawConstant(name).value.replace(/_/g, ""));
}

/** A live pool setting (cooldown, stake bounds, pool cap, ...): `name` is the IDL constant without
 *  its `SETTING_` prefix. Settings change through the program's timelock, so never use a constant. */
export function setting(pool: { settings: bigint[] }, name: string): bigint {
  return pool.settings[Number(num(`SETTING_${name}`))];
}

/** A byte-string constant from the IDL (PDA seeds, the approval domain). */
export function bytes(name: string): Uint8Array {
  return new Uint8Array(JSON.parse(rawConstant(name).value) as number[]);
}

export function explorerTx(sig: string): string {
  const q = IS_LOCALNET ? `cluster=custom&customUrl=${encodeURIComponent(POOL.rpcUrl)}` : `cluster=${POOL.cluster}`;
  return `https://explorer.solana.com/tx/${sig}?${q}`;
}
