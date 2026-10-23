// Chain reads for the panels: the pool, a stake, a backing, a claim. Layouts come from the IDL; yield
// and what a backer can collect come from the program's own views (`view_staker`, `view_backer`).
import { num, setting } from "./pool";
import { backerAddress, hasVenue, poolAddress, readAccount, refreshReserveIx, stakeAddress, view } from "./program";
import type { Accounts, PoolRecord } from "./program";

export type { PoolRecord };

/** Re-read after a confirmed send: now, then at 2 s and 5 s. The RPC node answering a read can be a
 *  moment behind the one that confirmed, so one read could still show the old state (B8 run 6). */
export function refreshSoon(bump: () => void): void {
  bump();
  setTimeout(bump, 2000);
  setTimeout(bump, 5000);
}

export type StakeRecord = {
  staker: string; beneficiary: string; amount: bigint; staked_at: bigint;
  forfeited: boolean; suspended: boolean; yield_owed: bigint;
  active_claim: string | null; reserved_claim: string | null;
};

export type PendingDeposit = { amount: bigint; matures_at: bigint };

export type BackerRecord = {
  backer: string; amount: bigint; pending: PendingDeposit[]; pending_count: number;
  withdraw_amount: bigint; withdraw_paid: bigint; yield_owed: bigint;
};

export type ClaimStatus =
  | "Unused" | "Reserved" | "PendingTime" | "AwaitingApproval" | "Active" | "Completed" | "Cancelled" | "Expired";

export type ClaimRecord = {
  staker: string; entitlement: bigint; streamed: bigint;
  cooldown_ends: bigint; vesting_ends: bigint; tier: number; status: ClaimStatus; approve_deadline: bigint;
  queue_seq: bigint;
};

/** The views count yield not yet harvested when given the venue's reserve; klend's refresh runs first
 *  in the same simulation, so that growth is at the current rate, not the last refresh's. */
const viewAccounts = (pool: PoolRecord, record: Record<string, string>): Accounts =>
  hasVenue(pool) ? { ...record, reserve: pool.yield_venue } : record;
const refreshFirst = async (pool: PoolRecord) => {
  const ix = await refreshReserveIx(pool).catch(() => null);
  return ix ? [ix] : [];
};

export async function readPool(): Promise<PoolRecord | null> {
  return readAccount<PoolRecord>("Pool", await poolAddress());
}

export type MyStake = StakeRecord & { yieldOwed: bigint };

export async function readMyStake(pool: PoolRecord, staker: string): Promise<MyStake | null> {
  const stake_record = await stakeAddress(staker);
  const s = await readAccount<StakeRecord>("StakeRecord", stake_record);
  if (!s) return null;
  const live = !s.forfeited && s.amount > 0n;
  const v = live ? await view<{ yield_owed: bigint }>("view_staker", viewAccounts(pool, { stake_record }), staker, await refreshFirst(pool)).catch(() => null) : null;
  return { ...s, yieldOwed: v?.yield_owed ?? s.yield_owed };
}

export type MyBacking = BackerRecord & {
  yieldOwed: bigint;
  /** What "Collect" pays now from an open withdrawal (the day's drip). */
  collectable: bigint;
  /** Deposits not yet counting, oldest first. */
  maturing: PendingDeposit[];
};

export async function readMyBacking(pool: PoolRecord, backer: string): Promise<MyBacking | null> {
  const backer_record = await backerAddress(backer);
  const b = await readAccount<BackerRecord>("BackerRecord", backer_record);
  if (!b) return null;
  const v = await view<{ yield_owed: bigint; collectable: bigint }>("view_backer", viewAccounts(pool, { backer_record }), backer, await refreshFirst(pool)).catch(() => null);
  return { ...b, yieldOwed: v?.yield_owed ?? b.yield_owed, collectable: v?.collectable ?? 0n, maturing: b.pending.slice(0, b.pending_count) };
}

export type MyClaim = { address: string; data: ClaimRecord };

/** The claim on this stake: the admitted one if any, else the queued one. */
export async function readMyClaim(stake: StakeRecord | null): Promise<MyClaim | null> {
  const a = stake?.active_claim ?? stake?.reserved_claim;
  if (!a) return null;
  const data = await readAccount<ClaimRecord>("Claim", a);
  return data ? { address: a, data } : null;
}

// Display only, below: what the panels show before a call. The program decides every call itself;
// these never gate anything it would accept.

/** pool-core claim::vested: linear from cooldown end to vesting end. */
export function vested(c: ClaimRecord, now: number): bigint {
  const start = Number(c.cooldown_ends);
  const end = Number(c.vesting_ends);
  if (end <= start) return now >= end ? c.entitlement : 0n;
  const elapsed = Math.max(0, Math.min(now, end) - start);
  return (c.entitlement * BigInt(elapsed)) / BigInt(end - start);
}

/** Stake bounds: pool cap x min/max stake bps, all live settings (pool-core stake::bounds). Above
 *  the max, the rest of a deposit goes in as backing. */
export function stakeBounds(pool: PoolRecord): { min: bigint; max: bigint } {
  return boundsOf(setting(pool, "POOL_CAP"), setting(pool, "MIN_STAKE_BPS"), setting(pool, "MAX_STAKE_BPS"));
}

/** Same cases as the program's (crates/pool-core/tests/vectors/stake_bounds.txt). */
export function boundsOf(cap: bigint, minBps: bigint, maxBps: bigint): { min: bigint; max: bigint } {
  const bps = num("BPS_DENOMINATOR");
  return { min: (cap * minBps) / bps, max: (cap * maxBps) / bps };
}

/** What a withdrawal of `amount` leaves, or why it can't go (pool-core stake::check_withdraw). */
export function withdrawCheck(pool: PoolRecord, stake: bigint, amount: bigint): string | null {
  if (amount <= 0n) return "Enter an amount above zero.";
  if (amount > stake) return "That's more than your stake.";
  const left = stake - amount;
  const { min } = stakeBounds(pool);
  if (left > 0n && left < min) return `What stays must be at least ${fmtUsdc(min)} USDC. Take it all out instead.`;
  return null;
}

const DECIMALS = Number(num("USDC_DECIMALS"));
const UNIT = num("USDC_UNIT");

/** USDC base units -> "1,234.56" (`dp` decimals, cut, not rounded). */
export function fmtUsdc(units: bigint, dp = 2): string {
  const neg = units < 0n;
  const v = neg ? -units : units;
  const frac = (v % UNIT).toString().padStart(DECIMALS, "0").slice(0, dp);
  return `${neg ? "-" : ""}${(v / UNIT).toLocaleString()}${dp ? "." + frac : ""}`;
}

/** "1.5" -> USDC base units, exact (no float). Null on anything that isn't a plain positive decimal. */
export function parseUsdc(s: string): bigint | null {
  const m = new RegExp(`^\\s*(\\d*)(?:\\.(\\d{0,${DECIMALS}}))?\\s*$`).exec(s);
  if (!m || (!m[1] && !m[2])) return null;
  return BigInt(m[1] || "0") * UNIT + BigInt((m[2] || "").padEnd(DECIMALS, "0"));
}

export function fmtDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 120) return `${s} seconds`;
  if (s < 7200) return `${Math.round(s / 60)} minutes`;
  if (s < 172800) return `${Math.round(s / 3600)} hours`;
  return `${Math.round(s / 86400)} days`;
}

// --- covered wallets (display list) --------------------------------------------------------------
// The program only stores wallet hashes, so the addresses shown here are kept in this browser, per
// staker (same as the multichain site). The backend's registry is the real list.

// `chain` is missing on rows saved before Ethereum wallets were added: those are Solana.
export type CoveredWallet = { chain?: string; wallet: string; registeredAt: number };
const storageKey = (owner: string) => `safu_solana_covered_wallets:${owner}`;

export function readCoveredWallets(owner: string): CoveredWallet[] {
  try {
    const raw = localStorage.getItem(storageKey(owner));
    return raw ? (JSON.parse(raw) as CoveredWallet[]) : [];
  } catch {
    return [];
  }
}

export function rememberCoveredWallet(owner: string, chain: string, wallet: string, registeredAt: number): CoveredWallet {
  const row = { chain, wallet, registeredAt: registeredAt * 1000 };
  try {
    const cur = readCoveredWallets(owner);
    if (!cur.some((w) => w.wallet === wallet)) localStorage.setItem(storageKey(owner), JSON.stringify([...cur, row]));
  } catch {
    // display only
  }
  return row;
}
