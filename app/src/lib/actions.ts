// Every user action, one program instruction each, signed by the connected wallet. Every one carries
// the Kamino accounts (when it moves money) and the pool's upkeep accounts. Payouts (withdraw, yield,
// claim stream) go to the stake's beneficiary, which this site always sets to the staking wallet itself.
import { PROGRAM_ID } from "./pool";
import { backerAddress, cashAccounts, chainNow, idlIx, queueAddress, readAccount, send, stakeAddress, upkeepMetas, usdcAccount } from "./program";
import type { Accounts, PoolRecord, Signer } from "./program";
import { readPool, stakeBounds } from "./reads";
import type { ClaimRecord } from "./reads";

async function build(pool: PoolRecord, name: string, accounts: Accounts, args: Record<string, unknown>, money: boolean) {
  const cash = money ? await cashAccounts(pool) : {};
  return idlIx(name, { ...cash, ...accounts }, args, await upkeepMetas(pool, await chainNow()));
}

/** Sends one instruction. If the pool moved between the read and the landing (its claim line or watch
 *  list advanced), the program refuses with UpkeepAccountsMissing: read the pool again, resend once. */
async function run(w: Signer, pool: PoolRecord | null, name: string, accounts: Accounts, args: Record<string, unknown> = {}, money = true) {
  if (!w.address) throw new Error("Connect a wallet first.");
  if (!pool) throw new Error("The pool isn't available right now. Try again in a minute.");
  try {
    return await send(w, [await build(pool, name, accounts, args, money)]);
  } catch (e) {
    const fresh = e instanceof Error && e.message === "PROGRAM_ERROR:UpkeepAccountsMissing" ? await readPool() : null;
    if (!fresh) throw e;
    return send(w, [await build(fresh, name, accounts, args, money)]);
  }
}

const me = (w: Signer) => w.address as string;
const myUsdc = (w: Signer, pool: PoolRecord | null) => (pool ? usdcAccount(me(w), pool.usdc_mint) : Promise.resolve(""));

/** Above the max stake the rest goes in as backing, which needs the backer record. */
export const stake = async (w: Signer, pool: PoolRecord | null, amount: bigint) =>
  run(w, pool, "stake", {
    payer: me(w), staker: me(w), staker_usdc: await myUsdc(w, pool),
    backer_record: pool && amount > stakeBounds(pool).max ? await backerAddress(me(w)) : PROGRAM_ID,
  }, { amount, beneficiary: me(w) });
/** `amount` of principal (all of it, or a part that leaves at least the min stake) + all yield. */
export const withdraw = async (w: Signer, pool: PoolRecord | null, amount: bigint) =>
  run(w, pool, "withdraw", { staker: me(w), beneficiary_usdc: await myUsdc(w, pool) }, { amount });
export const claimYield = async (w: Signer, pool: PoolRecord | null) =>
  run(w, pool, "claim_yield", { staker: me(w), beneficiary_usdc: await myUsdc(w, pool) });

export const back = async (w: Signer, pool: PoolRecord | null, amount: bigint) =>
  run(w, pool, "back", { payer: me(w), backer: me(w), backer_usdc: await myUsdc(w, pool) }, { amount });
// Permissionless: no backer account in the instruction, so its record is passed in.
export const matureBacking = async (w: Signer, pool: PoolRecord | null) =>
  run(w, pool, "mature_backing", { backer_record: await backerAddress(me(w)) });
export const requestBackerWithdrawal = (w: Signer, pool: PoolRecord | null, amount: bigint) =>
  run(w, pool, "request_backer_withdrawal", { backer: me(w) }, { amount }, false);
export const cancelBackerWithdrawal = (w: Signer, pool: PoolRecord | null) =>
  run(w, pool, "cancel_backer_withdrawal", { backer: me(w) }, {}, false);
export const completeBackerWithdrawal = async (w: Signer, pool: PoolRecord | null) =>
  run(w, pool, "complete_backer_withdrawal", { backer: me(w), backer_usdc: await myUsdc(w, pool) });
export const claimBackerYield = async (w: Signer, pool: PoolRecord | null) =>
  run(w, pool, "claim_backer_yield", { backer: me(w), backer_usdc: await myUsdc(w, pool) });

// Claim steps. Moving a queued/pending claim forward is permissionless; the connected wallet just pays the fee.
export const releaseQueuedClaim = async (w: Signer, pool: PoolRecord | null, claim: string, c: ClaimRecord) => {
  const queue_entry = await queueAddress(c.queue_seq);
  const entry = await readAccount<{ payer: string }>("QueueEntry", queue_entry);
  if (!entry) throw new Error("This claim isn't waiting in line anymore. Refresh the page.");
  return run(w, pool, "try_release_queued_claim", { claim, queue_entry, stake_record: await stakeAddress(c.staker), rent_to: entry.payer }, {}, false);
};
export const unlockPendingClaim = async (w: Signer, pool: PoolRecord | null, claim: string, c: ClaimRecord) =>
  run(w, pool, "unlock_pending_claim", { claim, stake_record: await stakeAddress(c.staker) }, {}, false);
export const approveClaim = (w: Signer, pool: PoolRecord | null, claim: string) =>
  run(w, pool, "approve_claim", { staker: me(w), claim }, {}, false);
export const collectClaim = async (w: Signer, pool: PoolRecord | null, claim: string) =>
  run(w, pool, "claim_stream", { staker: me(w), claim, beneficiary_usdc: await myUsdc(w, pool) });
