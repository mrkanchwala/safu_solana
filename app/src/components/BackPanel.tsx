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
import { setting } from "../lib/pool";
import { fmtDuration, fmtUsdc, parseUsdc, readMyBacking, refreshSoon } from "../lib/reads";
import type { MyBacking } from "../lib/reads";
import { usePool } from "../lib/usePool";
import { TxStatus } from "./TxStatus";

// Back the pool tab: like Stake, without covered wallets. Backing adds capacity for claims and shares
// in the yield, no coverage. Each new deposit waits on its own clock before it counts (up to
// MAX_PENDING_DEPOSITS at once); "Count it now" moves the matured ones in (`mature_backing`, anyone).
// Taking it out: a request is paid a little each day, over a few days on a quiet pool and longer on a
// busy one (`DRIP_MIN_SECS`..`DRIP_MAX_SECS`); "Collect" takes what the day allows.

function when(ts: bigint, now: bigint): string {
  return ts <= now ? "now" : `in ${fmtDuration(Number(ts - now))}`;
}

export function BackPanel() {
  const client = useClient();
  const backer = client.address;
  const [refreshKey, setRefreshKey] = useState(0);
  const { pool, now: chainTime } = usePool(refreshKey);
  // Live settings.
  const DRIP_MIN = pool ? Number(setting(pool, "DRIP_MIN_SECS")) : 0;
  const DRIP_MAX = pool ? Number(setting(pool, "DRIP_MAX_SECS")) : 0;
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

  const done = <T,>(p: Promise<T>) => p.finally(() => refreshSoon(() => setRefreshKey((k) => k + 1)));
  const units = parseUsdc(amount);
  const backAction = useAction(() => done(back(client, pool, units ?? 0n).finally(() => setAmount(""))));
  const matureAction = useAction(() => done(matureBacking(client, pool)));
  const requestAction = useAction(() => done(requestBackerWithdrawal(client, pool, backing?.amount ?? 0n)));
  const cancelAction = useAction(() => done(cancelBackerWithdrawal(client, pool)));
  const completeAction = useAction(() => done(completeBackerWithdrawal(client, pool)));
  const takeYieldAction = useAction(() => done(claimBackerYield(client, pool)));

  const now = BigInt(chainTime);
  const maturing = backing?.maturing ?? [];
  const maturingTotal = maturing.reduce((s, d) => s + d.amount, 0n);
  const nextCounts = maturing.reduce<bigint | null>((m, d) => (m === null || d.matures_at < m ? d.matures_at : m), null);
  const anyMatured = maturing.some((d) => d.matures_at <= now);
  const hasRequest = !!backing && backing.withdraw_amount > 0n;
  const canRequest = !!backing && backing.amount > 0n && !hasRequest;

  return (
    <div className="panel">
      <div>
        <div className="field-group">
          <div className="field-label">
            <span>Back the pool</span>
            <span>no covered wallets, no coverage</span>
          </div>
          <div className="field-input">
            <input placeholder="0" value={amount} onChange={(e) => setAmount(e.target.value)} inputMode="decimal" aria-label="Backing amount in USDC" />
            <span className="unit">USDC</span>
          </div>
        </div>
        <button className="primary-action" disabled={!backer || !units || backAction.isRunning} onClick={() => backAction.dispatch()}>
          {backAction.isRunning ? "Backing..." : "Back the pool"}
        </button>
        <TxStatus action={backAction} />
        <div className="ramp-disclosure" style={{ marginTop: 8 }}>
          <b>1. Back.</b> Each deposit starts counting after its own short wait.
          <br />
          <b>2. Take it out.</b> Press "Request withdrawal". It's paid out a little each day, over{" "}
          {fmtDuration(DRIP_MIN)} when the pool is quiet and up to {fmtDuration(DRIP_MAX)} when claims are using
          much of it. Press "Collect" to take what's ready.
          <br />
          Why the waits: nobody can add money just before a claim they know is coming, or pull it all out
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
                <div className="v">{fmtUsdc(backing.amount)} USDC</div>
              </div>
              {maturingTotal > 0n && nextCounts !== null ? (
                <div className="side-stat">
                  <div className="k">Waiting to count</div>
                  <div className="v">
                    {fmtUsdc(maturingTotal)} USDC · {anyMatured ? "ready to count" : `next counts ${when(nextCounts, now)}`}
                  </div>
                </div>
              ) : null}
              {backing.yieldOwed > 0n ? (
                <div className="side-stat">
                  <div className="k">Yield earned</div>
                  <div className="v">{fmtUsdc(backing.yieldOwed, 6)} USDC</div>
                </div>
              ) : null}
              {hasRequest ? (
                <div className="side-stat">
                  <div className="k">Withdrawal requested</div>
                  <div className="v">
                    {fmtUsdc(backing.withdraw_paid)} of {fmtUsdc(backing.withdraw_amount)} USDC paid ·{" "}
                    {backing.collectable > 0n ? `${fmtUsdc(backing.collectable)} USDC ready` : "next part tomorrow"}
                  </div>
                </div>
              ) : null}
            </>
          )}
        </div>

        {anyMatured ? (
          <button className="secondary-action" style={{ width: "100%" }} disabled={matureAction.isRunning} onClick={() => matureAction.dispatch()}>
            {matureAction.isRunning ? "Updating..." : "Count it now"}
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
            <button className="secondary-action" style={{ width: "100%" }} disabled={backing.collectable <= 0n || completeAction.isRunning} onClick={() => completeAction.dispatch()}>
              {completeAction.isRunning ? "Collecting..." : "Collect"}
            </button>
            <button className="secondary-action" style={{ width: "100%", marginTop: 8 }} disabled={cancelAction.isRunning} onClick={() => cancelAction.dispatch()}>
              {cancelAction.isRunning ? "Cancelling..." : "Cancel the rest"}
            </button>
          </>
        ) : null}
        <TxStatus action={completeAction} />
        <TxStatus action={cancelAction} />
        {backing && backing.yieldOwed > 0n ? (
          <button className="secondary-action" style={{ width: "100%", marginTop: 8 }} disabled={takeYieldAction.isRunning} onClick={() => takeYieldAction.dispatch()}>
            {takeYieldAction.isRunning ? "Sending..." : `Take ${fmtUsdc(backing.yieldOwed, 6)} USDC yield`}
          </button>
        ) : null}
        <TxStatus action={takeYieldAction} />
      </div>

      <div>
        <div className="side-stat">
          <div className="k">Total backed</div>
          <div className="v">{pool ? fmtUsdc(pool.total_backed) : "--"} USDC</div>
        </div>
        <div className="side-stat">
          <div className="k">What backing does</div>
          <div className="v good" style={{ fontFamily: "var(--font-body)", fontSize: 14 }}>
            Backing adds room for claims to be paid, and shares in the yield the pool's USDC earns on Kamino.
          </div>
        </div>
      </div>
    </div>
  );
}
