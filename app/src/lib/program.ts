// The safu_pool program client. Every instruction is built from the IDL: its accounts (in IDL order,
// with the IDL's writable/signer flags), its PDA seeds, and its argument encoding. The Kamino accounts
// come from the pool record and the reserve itself (offsets and ids are IDL `KLEND_*` constants), the
// upkeep accounts from the pool's claim line and watch list, as backend/solana_pool.py builds them.
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  compileTransaction,
  createSolanaRpc,
  createTransactionMessage,
  getAddressEncoder,
  getAddressDecoder,
  getBase64EncodedWireTransaction,
  getBase64Encoder,
  getProgramDerivedAddress,
  getSignatureFromTransaction,
  getTransactionDecoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
} from "@solana/kit";
import type { Address, Instruction, Transaction } from "@solana/kit";
import { getSetComputeUnitLimitInstruction } from "@solana-program/compute-budget";
import { IDL, POOL, PROGRAM_ID, bytes, num, pubkey } from "./pool";
import type { IdlAccountItem } from "./pool";
import { decodeAccount, decodeReturn, encodeIx, programErrorName } from "./idl";

// The deployed site reads through its own /rpc (keys and failover stay on the server; the public
// devnet RPC limits each IP). Local dev, tests and localnet use rpcUrl directly.
const siteRpc = POOL.siteRpcPath && typeof window !== "undefined" && !import.meta.env.DEV ? new URL(POOL.siteRpcPath, window.location.origin).href : null;
export const rpc = createSolanaRpc(siteRpc ?? POOL.rpcUrl);

const SYSTEM_PROGRAM_ID = "11111111111111111111111111111111"; // hardcode-ok: Solana built-in program id, same on every cluster
const TOKEN_PROGRAM_ID = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"; // hardcode-ok: Solana built-in program id, same on every cluster
const TOKEN_2022_PROGRAM_ID = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"; // hardcode-ok: Solana built-in program id, same on every cluster
const ATA_PROGRAM_ID = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"; // hardcode-ok: Solana built-in program id, same on every cluster
const SYSVAR_INSTRUCTIONS_ID = "Sysvar1nstructions1111111111111111111111111"; // hardcode-ok: Solana built-in sysvar, same on every cluster
const addrEnc = getAddressEncoder();
const addrDec = getAddressDecoder();

export type Accounts = Record<string, string>;
type Meta = { address: Address; role: AccountRole };

async function pda(seeds: Uint8Array[], program: string = PROGRAM_ID): Promise<Address> {
  const [a] = await getProgramDerivedAddress({ programAddress: address(program), seeds });
  return a;
}

const key = (a: string) => new Uint8Array(addrEnc.encode(address(a)));
const u64le = (n: bigint) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, true);
  return b;
};

export const poolAddress = () => pda([bytes("SEED_POOL")]);
export const stakeAddress = async (staker: string) => pda([bytes("SEED_STAKE"), key(await poolAddress()), key(staker)]);
export const backerAddress = async (backer: string) => pda([bytes("SEED_BACKER"), key(await poolAddress()), key(backer)]);
export const queueAddress = async (seq: bigint) => pda([bytes("SEED_QUEUE"), key(await poolAddress()), u64le(seq)]);
const watchAddress = async (seq: bigint) => pda([bytes("SEED_WATCH"), key(await poolAddress()), u64le(seq)]);

/** The owner's USDC account (the associated token account). The program takes any USDC account the
 *  owner holds; the site always uses this one. */
export const usdcAccount = (owner: string, mint: string) =>
  pda([key(owner), key(TOKEN_PROGRAM_ID), key(mint)], ATA_PROGRAM_ID);

// --- reads ----------------------------------------------------------------------------------------

export type RawAccount = { data: Uint8Array; lamports: bigint; owner: string };

export async function fetchAccount(a: string): Promise<RawAccount | null> {
  const { value } = await rpc.getAccountInfo(address(a), { encoding: "base64", commitment: "confirmed" }).send();
  if (!value) return null;
  return {
    data: new Uint8Array(getBase64Encoder().encode(value.data[0])),
    lamports: BigInt(value.lamports),
    owner: value.owner,
  };
}

/** A program account decoded by its IDL layout; null if it doesn't exist. Refuses an account the
 *  program doesn't own (same check as the backend's _account_data). */
export async function readAccount<T>(name: string, a: string): Promise<T | null> {
  const acc = await fetchAccount(a);
  if (!acc) return null;
  if (acc.owner !== PROGRAM_ID) throw new Error(`${a} is not owned by the SAFU program`);
  return decodeAccount<T>(name, acc.data);
}

/** The chain's own clock (unix seconds), which can run behind the browser's. Every "ready at" check
 *  uses it, so a button never turns on before the program would accept the call. */
export async function chainNow(): Promise<number> {
  const slot = await rpc.getSlot({ commitment: "confirmed" }).send();
  const t = await rpc.getBlockTime(slot).send();
  return t === null ? Math.floor(Date.now() / 1000) : Number(t);
}

export type PoolRecord = {
  usdc_mint: string; yield_venue: string; paused_until: bigint;
  /** Live adjustable settings, one slot per IDL `SETTING_*` constant: read them with `setting()`. */
  settings: bigint[];
  total_staked: bigint; total_stakers: bigint; total_backed: bigint;
  queue_head: bigint; queue_tail: bigint; watch_head: bigint; watch_tail: bigint; watch_cursor: bigint;
};

const keyAt = (b: Uint8Array, o: number) => addrDec.decode(b.subarray(o, o + 32));

/** The pool has a yield venue (an unset key is all zeros, which is the System program's id). */
export const hasVenue = (pool: PoolRecord) => pool.yield_venue !== SYSTEM_PROGRAM_ID;

// --- Kamino (the `cash` group every money-moving instruction takes) -------------------------------

/** The mint, then the yield venue's accounts read from its reserve (backend solana_relay.venue_accounts).
 *  Without a venue the optional ones stay out (idlIx passes the program id in their place). */
export async function cashAccounts(pool: PoolRecord): Promise<Accounts> {
  const base = { usdc_mint: pool.usdc_mint };
  if (!hasVenue(pool)) return base;
  const klend = pubkey("KLEND_PROGRAM_ID");
  const reserve = await fetchAccount(pool.yield_venue);
  if (!reserve || reserve.owner !== klend) throw new Error("The pool's lending venue can't be read right now. Nothing was lost. Try again in a few minutes.");
  const at = (name: string) => keyAt(reserve.data, Number(num(name)));
  const set = (name: string) => {
    const k = at(name);
    return k === SYSTEM_PROGRAM_ID || k === pubkey("KLEND_NULL_KEY") ? null : k;
  };
  const market = at("KLEND_RESERVE_LENDING_MARKET");
  const collateral = at("KLEND_RESERVE_COLLATERAL_MINT");
  const oracle = set("KLEND_RESERVE_SCOPE_FEED") ?? set("KLEND_RESERVE_PYTH");
  return {
    ...base,
    klend_program: klend,
    reserve: pool.yield_venue,
    lending_market: market,
    lending_market_authority: await pda([bytes("KLEND_LENDING_MARKET_AUTH_SEED"), key(market)], klend),
    reserve_liquidity_supply: at("KLEND_RESERVE_LIQUIDITY_SUPPLY"),
    reserve_collateral_mint: collateral,
    pool_collateral: await pda([bytes("SEED_LEG"), key(await poolAddress()), key(collateral)]),
    ...(oracle ? { oracle } : {}),
    instructions: SYSVAR_INSTRUCTIONS_ID,
  };
}

// --- upkeep (the remaining accounts every user instruction carries) --------------------------------

/** pool_core::upkeep::watch_places: from the cursor on, wrapping to the head, each at most once. */
function watchPlaces(head: bigint, tail: bigint, cursor: bigint): bigint[] {
  if (head >= tail) return [];
  const norm = (c: bigint) => (c < head || c >= tail ? head : c);
  const out: bigint[] = [];
  let pos = norm(cursor);
  for (let i = 0n; i < num("UPKEEP_WATCH_PER_CALL"); i++) {
    if (out.includes(pos)) break;
    out.push(pos);
    pos = norm(pos + 1n);
  }
  return out;
}

/** `WatchEntry` only travels as an upkeep account, so Anchor leaves it out of the IDL. Its layout is
 *  state.rs `WatchEntry`: claim, payer, after Anchor's discriminator (same read as the backend). */
async function readWatchEntry(a: string): Promise<{ claim: string; payer: string } | null> {
  const acc = await fetchAccount(a);
  if (!acc || acc.owner !== PROGRAM_ID) return null;
  const disc = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("account:WatchEntry"))).subarray(0, 8);
  if (!disc.every((x, i) => x === acc.data[i])) return null;
  return { claim: keyAt(acc.data, 8), payer: keyAt(acc.data, 40) };
}

/** The head of the claim line, then the watch places, 4 writable accounts each (place, its claim,
 *  that claim's stake, the place's rent payer). None while paused. If the pool moves before the
 *  transaction lands, the program refuses with UpkeepAccountsMissing and actions.ts resends once. */
export async function upkeepMetas(pool: PoolRecord, now: number): Promise<Meta[]> {
  if (BigInt(now) < pool.paused_until) return [];
  const places: [string, { claim: string; payer: string } | null][] = [];
  if (pool.queue_head < pool.queue_tail) {
    const q = await queueAddress(pool.queue_head);
    places.push([q, await readAccount<{ claim: string; payer: string }>("QueueEntry", q)]);
  }
  for (const pos of watchPlaces(pool.watch_head, pool.watch_tail, pool.watch_cursor)) {
    const w = await watchAddress(pos);
    places.push([w, await readWatchEntry(w)]);
  }
  const metas: Meta[] = [];
  for (const [place, entry] of places) {
    const claim = entry ? await readAccount<{ staker: string }>("Claim", entry.claim) : null;
    if (!entry || !claim) throw new Error("The pool moved while this was being prepared. Nothing was lost. Try again.");
    for (const a of [place, entry.claim, await stakeAddress(claim.staker), entry.payer]) {
      metas.push({ address: address(a), role: AccountRole.WRITABLE });
    }
  }
  return metas;
}

// --- instructions ---------------------------------------------------------------------------------

async function resolve(item: IdlAccountItem, given: Accounts, done: Accounts): Promise<string> {
  if (given[item.name]) return given[item.name];
  if (item.address) return item.address;
  if (item.optional) return PROGRAM_ID;
  if (item.pda) {
    const seeds: Uint8Array[] = [];
    for (const s of item.pda.seeds) {
      if (s.kind === "const") seeds.push(new Uint8Array(s.value));
      else if (s.kind === "account" && !s.path.includes(".") && (done[s.path] || given[s.path])) seeds.push(key(done[s.path] || given[s.path]));
      else throw new Error(`account ${item.name}: pass it in (seed ${s.path} can't be derived here)`);
    }
    return pda(seeds);
  }
  throw new Error(`account ${item.name} missing`);
}

/** One program instruction. `given` names accounts the IDL can't derive (signers, token accounts, the
 *  claim, the Kamino accounts); everything else comes from the IDL's fixed addresses and PDA seeds. An
 *  optional account left out is the program id, read-only (as Anchor's clients pass it). */
export async function idlIx(name: string, given: Accounts, args: Record<string, unknown> = {}, remaining: Meta[] = []): Promise<Instruction> {
  const ix = IDL.instructions.find((i) => i.name === name);
  if (!ix) throw new Error(`IDL has no instruction ${name}`);
  const done: Accounts = {};
  const metas: Meta[] = [];
  const walk = async (items: IdlAccountItem[]) => {
    for (const item of items) {
      if (item.accounts) {
        await walk(item.accounts);
        continue;
      }
      const a = await resolve(item, given, done);
      done[item.name] = a;
      const absent = item.optional && a === PROGRAM_ID;
      const role = absent
        ? AccountRole.READONLY
        : item.signer
          ? item.writable ? AccountRole.WRITABLE_SIGNER : AccountRole.READONLY_SIGNER
          : item.writable ? AccountRole.WRITABLE : AccountRole.READONLY;
      metas.push({ address: address(a), role });
    }
  };
  await walk(ix.accounts);
  return { programAddress: address(PROGRAM_ID), accounts: [...metas, ...remaining], data: encodeIx(name, args) };
}

/** A view (`view_staker`, `view_backer`) run by simulation: nothing is signed or written. The fee
 *  payer only has to exist, so the connected wallet is used. */
export async function view<T>(name: string, given: Accounts, feePayer: string): Promise<T> {
  const ix = await idlIx(name, given);
  const { value: blockhash } = await rpc.getLatestBlockhash({ commitment: "confirmed" }).send();
  const msg = pipe(
    createTransactionMessage({ version: "legacy" }),
    (m) => setTransactionMessageFeePayer(address(feePayer), m),
    (m) => setTransactionMessageLifetimeUsingBlockhash(blockhash, m),
    (m) => appendTransactionMessageInstructions([ix], m),
  );
  const wire = getBase64EncodedWireTransaction(compileTransaction(msg));
  const { value } = await rpc
    .simulateTransaction(wire, { encoding: "base64", sigVerify: false, replaceRecentBlockhash: true, commitment: "confirmed" })
    .send();
  const data = value.returnData?.data[0];
  if (value.err || !data) throw new Error(`view ${name} failed`);
  return decodeReturn<T>(name, new Uint8Array(getBase64Encoder().encode(data)));
}

// --- send -----------------------------------------------------------------------------------------

export type Signer = { address: string | null; solanaSignTransaction: (tx: Uint8Array) => Promise<Uint8Array> };

/** A failed simulation as `PROGRAM_ERROR:<IDL error name>` when this program refused it, or
 *  `VENUE_REFUSED` when Kamino (called by the pool) did; friendly-error.ts turns either into a
 *  sentence. A custom error number only means something for the program that raised it: the first
 *  "Program <id> failed" line in the logs names that program (the innermost call fails first). */
function explain(e: unknown): Error {
  const seen = new Set<unknown>();
  const texts: string[] = [];
  const collect = (x: unknown) => {
    if (!x || typeof x !== "object" || seen.has(x)) return;
    seen.add(x);
    const o = x as { message?: string; context?: Record<string, unknown>; cause?: unknown };
    if (o.message) texts.push(o.message);
    for (const v of Object.values(o.context ?? {})) {
      if (Array.isArray(v)) texts.push(...v.filter((l): l is string => typeof l === "string"));
      else if (typeof v === "string") texts.push(v);
      else collect(v);
    }
    collect(o.cause);
  };
  collect(e);
  // A USDC account the wallet never opened (Anchor refuses it before the program runs).
  if (texts.some((t) => /Error Code: AccountNotInitialized/.test(t))) return new Error("NO_USDC_ACCOUNT");
  const failed = texts.map((t) => /Program (\w+) failed: custom program error: 0x([0-9a-f]+)/i.exec(t)).find(Boolean);
  if (failed && failed[1] !== PROGRAM_ID) {
    // Error 0x1 is "not enough funds": SOL for the fee from the System program, USDC from a token program.
    if (failed[2] === "1" && failed[1] === SYSTEM_PROGRAM_ID) return new Error(`insufficient funds: ${failed[0]}`);
    if (failed[2] === "1" && [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID].includes(failed[1])) return new Error(`insufficient USDC: ${failed[0]}`);
    if (failed[1] === pubkey("KLEND_PROGRAM_ID")) return new Error(`VENUE_REFUSED: ${failed[0]}`);
    return new Error(`OTHER_PROGRAM_REFUSED: ${failed[0]}`);
  }
  for (const t of texts) {
    const m = /Program log: AnchorError .*Error Code: (\w+)\./.exec(t);
    if (m && (!failed || failed[1] === PROGRAM_ID) && IDL.errors?.some((x) => x.name === m[1])) return new Error(`PROGRAM_ERROR:${m[1]}`);
  }
  if (failed) {
    const name = programErrorName(parseInt(failed[2], 16));
    if (name) return new Error(`PROGRAM_ERROR:${name}`);
  }
  return e instanceof Error ? e : new Error(String(e));
}

/** Builds, has the wallet sign, sends and confirms. Returns the transaction signature. */
export async function send(wallet: Signer, ixs: Instruction[]): Promise<string> {
  if (!wallet.address) throw new Error("Connect a wallet first.");
  const { value: blockhash } = await rpc.getLatestBlockhash({ commitment: "confirmed" }).send();
  const msg = pipe(
    createTransactionMessage({ version: "legacy" }),
    (m) => setTransactionMessageFeePayer(address(wallet.address!), m),
    (m) => setTransactionMessageLifetimeUsingBlockhash(blockhash, m),
    (m) => appendTransactionMessageInstructions([getSetComputeUnitLimitInstruction({ units: POOL.computeUnitLimit }), ...ixs], m),
  );
  const signedBytes = await wallet.solanaSignTransaction(new Uint8Array(getTransactionEncoder().encode(compileTransaction(msg))));
  const signed = getTransactionDecoder().decode(signedBytes) as Transaction;
  const sig = getSignatureFromTransaction(signed as Parameters<typeof getSignatureFromTransaction>[0]);
  try {
    await rpc.sendTransaction(getBase64EncodedWireTransaction(signed), { encoding: "base64", preflightCommitment: "confirmed" }).send();
  } catch (e) {
    throw explain(e);
  }
  for (let i = 0; i < 60; i++) {
    const s = (await rpc.getSignatureStatuses([sig]).send()).value[0];
    if (s && (s.confirmationStatus === "confirmed" || s.confirmationStatus === "finalized")) {
      if (s.err) throw new Error("The transaction failed on-chain. Nothing moved. Try again.");
      return sig;
    }
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error("The transaction wasn't confirmed in time. Check your wallet before trying again, so it doesn't happen twice.");
}
