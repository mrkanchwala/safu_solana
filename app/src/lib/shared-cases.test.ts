// The site's copies of pool-core rules, run against the program's own cases
// (crates/pool-core/tests/vectors), and the largest transaction the site builds against Solana's limit.
import { readFileSync } from "node:fs";
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  compileTransaction,
  createTransactionMessage,
  getAddressDecoder,
  getTransactionEncoder,
  pipe,
  setTransactionMessageFeePayer,
  setTransactionMessageLifetimeUsingBlockhash,
} from "@solana/kit";
import type { Blockhash } from "@solana/kit";
import { describe, expect, it } from "vitest";

import { IDL, num, pubkey } from "./pool";
import { decodeWatchEntry, idlIx, priceSource, watchPlaces, withBudget } from "./program";
import { boundsOf } from "./reads";

const vector = (name: string) =>
  readFileSync(new URL(`../../../crates/pool-core/tests/vectors/${name}`, import.meta.url), "utf8")
    .split("\n")
    .filter((l) => l.trim() && !l.startsWith("#"))
    .map((l) => l.split(":").map((p) => p.trim().split(/\s+/).filter(Boolean)));

const addrDec = getAddressDecoder();
const addr = (fill: number) => addrDec.decode(new Uint8Array(32).fill(fill));
let next = 1;
const fresh = () => {
  const b = new Uint8Array(32);
  new DataView(b.buffer).setUint32(0, next++);
  return addrDec.decode(b);
};

describe("same cases as the program", () => {
  it("watch places", () => {
    const cases = vector("watch_places.txt");
    for (const [[h, t, c], want] of cases) {
      expect(watchPlaces(BigInt(h), BigInt(t), BigInt(c)), `${h} ${t} ${c}`).toEqual(want.map(BigInt));
    }
    expect(cases.length).toBeGreaterThanOrEqual(7);
  });

  it("stake bounds", () => {
    const cases = vector("stake_bounds.txt");
    for (const [[cap, lo, hi], [min, max]] of cases) {
      expect(boundsOf(BigInt(cap), BigInt(lo), BigInt(hi))).toEqual({ min: BigInt(min), max: BigInt(max) });
    }
    expect(cases.length).toBeGreaterThanOrEqual(4);
  });

  it("price source", () => {
    const key = (s: string) => (s === "-" ? addr(0) : s === "nu11" ? pubkey("KLEND_NULL_KEY") : addr(s.charCodeAt(0)));
    const cases = vector("price_source.txt");
    for (const [[scope, pyth, sbp, sbt], [want]] of cases) {
      const got = priceSource({ scope: key(scope), pyth: key(pyth), switchboardPrice: key(sbp), switchboardTwap: key(sbt) });
      expect(got?.account ?? "refused", `${scope} ${pyth} ${sbp} ${sbt}`).toBe(want === "refused" ? "refused" : key(want));
    }
    expect(cases.length).toBeGreaterThanOrEqual(8);
  });

  it("WatchEntry read at the IDL's offsets", async () => {
    const claimAt = Number(num("WATCH_ENTRY_CLAIM_AT"));
    const data = new Uint8Array(Number(num("WATCH_ENTRY_DONE_AT")) + 2);
    const disc = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode("account:WatchEntry")));
    data.set(disc.subarray(0, claimAt));
    data.fill(1, claimAt, claimAt + 32);
    data.fill(2, Number(num("WATCH_ENTRY_PAYER_AT")), Number(num("WATCH_ENTRY_PAYER_AT")) + 32);
    const acc = { data, lamports: 1n, owner: IDL.address };
    expect(await decodeWatchEntry(acc)).toEqual({ claim: addr(1), payer: addr(2) });
    expect(await decodeWatchEntry({ ...acc, data: new Uint8Array(data.length) })).toBeNull();
  });
});

describe("transaction size", () => {
  // Every instruction the site sends, every account a distinct address (no venue account shared), with
  // the claim line's head and the full watch list as upkeep accounts: the worst case.
  const ARGS: Record<string, Record<string, unknown>> = {
    stake: { amount: 1n, beneficiary: addr(9) },
    withdraw: { amount: 1n },
    back: { amount: 1n },
    request_backer_withdrawal: { amount: 1n },
  };
  const NAMES = [
    "stake", "withdraw", "claim_yield", "back", "mature_backing", "request_backer_withdrawal",
    "cancel_backer_withdrawal", "complete_backer_withdrawal", "claim_backer_yield", "try_release_queued_claim",
    "unlock_pending_claim", "approve_claim", "claim_stream",
  ];
  const LIMIT = 1232; // hardcode-ok: Solana's packet size for a transaction

  it.each(NAMES)("%s fits a legacy transaction", async (name) => {
    // The connected wallet pays, signs and is the staker/backer, as the site sends it.
    const wallet = fresh();
    const mine = ["payer", "staker", "backer"];
    const given = new Proxy({} as Record<string, string>, { get: (t, k: string) => (t[k] ??= mine.includes(k) ? wallet : fresh()) });
    const places = 1 + Number(num("UPKEEP_WATCH_PER_CALL"));
    const upkeep = Array.from({ length: places * 4 }, () => ({ address: address(fresh()), role: AccountRole.WRITABLE }));
    const ix = await idlIx(name, given, ARGS[name] ?? {}, upkeep);
    const msg = pipe(
      createTransactionMessage({ version: "legacy" }),
      (m) => setTransactionMessageFeePayer(address(wallet), m),
      (m) => setTransactionMessageLifetimeUsingBlockhash({ blockhash: fresh() as unknown as Blockhash, lastValidBlockHeight: 0n }, m),
      (m) => appendTransactionMessageInstructions(withBudget([ix], 1n), m),
    );
    const size = getTransactionEncoder().encode(compileTransaction(msg)).length;
    expect(size, `${name}: ${size} bytes`).toBeLessThanOrEqual(LIMIT);
  });
});
