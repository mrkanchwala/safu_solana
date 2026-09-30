import { useEffect, useState } from "react";
import { useAction, useClient } from "../lib/client";
import {
  back,
  cancelBackerWithdrawal,
  claimBackerYield,
  completeBackerWithdrawal,
  matureBacking,
  requestBackerWithdrawal,
} from "../lib/actions";
import { num } from "../lib/pool";
import { fmtDuration, fmtSol, parseSol, readMyBacking } from "../lib/reads";
import type { MyBacking } from "../lib/reads";
import { usePool } from "../lib/usePool";
import { TxStatus } from "./TxStatus";

// Back the pool tab: like Stake, without covered wallets. Backing adds capacity for claims; it
// earns yield, no coverage. New backing matures before it counts; taking it out needs notice.
const MATURITY = Number(num("BACKER_MATURITY_SECS"));
const NOTICE = Number(num("BACKER_NOTICE_SECS"));

function when(ts: bigint, now: bigint): string {
  if (!ts) return "--";
  return ts <= now ? "now" : `in ${fmtDuration(Number(ts - now))}`;
}

export function BackPanel() {
  const client = useClient();
  const backer = client.address;
  const [refreshKey, setRefreshKey] = useState(0);
  const { pool, now: chainTime } = usePool(refreshKey);
  const [amount, setAmount] = useState("");
  const [backing, setBacking] = useState<MyBacking | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const b = pool && backer ? await readMyBacking(pool, backer).catch(() => null) : null;
      if (!cancelled) setBacking(b);
    })();
    return () => {
      cancelled = true;
    };
  }, [pool, backer]);

  const done = <T,>(p: Promise<T>) => p.finally(() => setRefreshKey((k) => k + 1));
  const lamports = parseSol(amount);
  const backAction = useAction(() => done(back(client, pool, lamports ?? 0n).finally(() => setAmount(""))));
  const matureAction = useAction(() => done(matureBacking(client, pool)));
  const requestAction = useAction(() => done(requestBackerWithdrawal(client, pool, backing?.amount ?? 0n)));
  const cancelAction = useAction(() => done(cancelBackerWithdrawal(client, pool)));
  const completeAction = useAction(() => done(completeBackerWithdrawal(client, pool)));
  const takeYieldAction = useAction(() => done(claimBackerYield(client, pool)));

  const now = BigInt(chainTime);
  const canMature = !!backing && backing.pending_amount > 0n && backing.pending_matures_at <= now;
  const canRequest = !!backing && backing.amount > 0n && backing.withdraw_amount === 0n;
  const hasRequest = !!backing && backing.withdraw_amount > 0n;
  const canComplete = hasRequest && backing.withdraw_ready_at <= now;

  return (
    <div className="panel">
      <div>
        <div className="field-group">
          <div className="field-label">
            <span>Back the pool</span>
            <span>no covered wallets, no coverage</span>
          </div>
          <div className="field-input">
            <input placeholder="0" value={amount} onChange={(e) => setAmount(e.target.value)} inputMode="decimal" aria-label="Backing amount in SOL" />
            <span className="unit">SOL</span>
          </div>
        </div>
        <button className="primary-action" disabled={!backer || !lamports || backAction.isRunning} onClick={() => backAction.dispatch()}>
          {backAction.isRunning ? "Backing..." : "Back the pool"}
        </button>
        <TxStatus action={backAction} />
        <div className="ramp-disclosure" style={{ marginTop: 8 }}>
          <b>1. Back.</b> New backing waits {fmtDuration(MATURITY)} before it can pay claims. When it's
          ready, press "Count my matured backing".
          <br />
          <b>2. Take it out.</b> Press "Request withdrawal", wait {fmtDuration(NOTICE)}, then press
          "Complete withdrawal".
          <br />
          Why the waits: nobody can add money just before a claim they know is coming, or pull it out
          right before a claim that's already on the way.
        </div>

        <div className="field-group" style={{ marginTop: 20 }}>
          <div className="field-label">
            <span>Your backing</span>
            <span>&nbsp;</span>
          </div>
          {!backing ? (
            <div className="side-stat">
              <div className="k">Status</div>
              <div className="v">No backing yet</div>
            </div>
          ) : (
            <>
              <div className="side-stat">
                <div className="k">Counting toward capacity</div>
                <div className="v">{fmtSol(backing.amount)} SOL</div>
              </div>
              {backing.pending_amount > 0n ? (
                <div className="side-stat">
                  <div className="k">Maturing</div>
                  <div className="v">{fmtSol(backing.pending_amount)} SOL · ready {when(backing.pending_matures_at, now)}</div>
                </div>
              ) : null}
              {backing.yieldOwed > 0n ? (
                <div className="side-stat">
                  <div className="k">Yield earned</div>
                  <div className="v">{fmtSol(backing.yieldOwed, 6)} SOL</div>
                </div>
              ) : null}
              {hasRequest ? (
                <div className="side-stat">
                  <div className="k">Withdrawal requested</div>
                  <div className="v">{fmtSol(backing.withdraw_amount)} SOL · ready {when(backing.withdraw_ready_at, now)}</div>
                </div>
              ) : null}
            </>
          )}
        </div>

        {canMature ? (
          <button className="secondary-action" style={{ width: "100%" }} disabled={matureAction.isRunning} onClick={() => matureAction.dispatch()}>
            {matureAction.isRunning ? "Updating..." : "Count my matured backing"}
          </button>
        ) : null}
        {/* Results sit outside the conditional blocks: each step hides its own button once done. */}
        <TxStatus action={matureAction} />
        {canRequest ? (
          <button className="secondary-action" style={{ width: "100%" }} disabled={requestAction.isRunning} onClick={() => requestAction.dispatch()}>
            {requestAction.isRunning ? "Requesting..." : "Request withdrawal"}
          </button>
        ) : null}
        <TxStatus action={requestAction} />
        {hasRequest ? (
          <>
            <button className="secondary-action" style={{ width: "100%" }} disabled={!canComplete || completeAction.isRunning} onClick={() => completeAction.dispatch()}>
              {completeAction.isRunning ? "Withdrawing..." : "Complete withdrawal"}
            </button>
            <button className="secondary-action" style={{ width: "100%", marginTop: 8 }} disabled={cancelAction.isRunning} onClick={() => cancelAction.dispatch()}>
              {cancelAction.isRunning ? "Cancelling..." : "Cancel request"}
            </button>
          </>
        ) : null}
        <TxStatus action={completeAction} />
        <TxStatus action={cancelAction} />
        {backing && backing.yieldOwed > 0n ? (
          <button className="secondary-action" style={{ width: "100%", marginTop: 8 }} disabled={takeYieldAction.isRunning} onClick={() => takeYieldAction.dispatch()}>
            {takeYieldAction.isRunning ? "Sending..." : `Take ${fmtSol(backing.yieldOwed, 6)} SOL yield`}
          </button>
        ) : null}
        <TxStatus action={takeYieldAction} />
      </div>

      <div>
        <div className="side-stat">
          <div className="k">Total backed</div>
          <div className="v">{pool ? fmtSol(pool.total_backed, 2) : "--"} SOL</div>
        </div>
        <div className="side-stat">
          <div className="k">What backing does</div>
          <div className="v good" style={{ fontFamily: "var(--font-body)", fontSize: 14 }}>
            Backing adds room for claims to be paid, and earns a share of the pool's staking yield.
          </div>
        </div>
      </div>
    </div>
  );
}
