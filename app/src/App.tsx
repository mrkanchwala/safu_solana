import { useState } from "react";
import { SiteHeader } from "./components/SiteHeader";
import { SiteFooter } from "./components/SiteFooter";
import { StakePanel } from "./components/StakePanel";
import { ClaimsPanel } from "./components/ClaimsPanel";
import { ClaimFilePanel } from "./components/ClaimFilePanel";
import { BackPanel } from "./components/BackPanel";
import { useClient } from "./lib/client";
import { POOL } from "./lib/pool";
import { fmtSol, stakeBounds } from "./lib/reads";
import { usePool } from "./lib/usePool";

type Tab = "stake" | "back" | "file" | "collect";

const TABS: { id: Tab; label: string }[] = [
  { id: "stake", label: "Stake" },
  { id: "back", label: "Back the pool" },
  { id: "file", label: "File a claim" },
  { id: "collect", label: "Collect payout" },
];

const NET = POOL.cluster === "devnet" ? "Solana devnet" : "a local Solana test chain";

export default function App() {
  const client = useClient();
  const [tab, setTab] = useState<Tab>("stake");
  const { pool, error } = usePool();

  // Shown only when the pool can't be read (founder, 2026-10-01: no standing devnet/demo banner).
  const badge = error ? <div className="devnet-badge">The pool isn't reachable on {NET} right now</div> : null;

  return (
    <>
      {badge}
      <SiteHeader client={client} view="main" />

      <section className="hero">
        <div className="wrap">
          <h1>
            SAFU pays your wallet back.
            <br />
            <em>Automatically. No vote. No appeal.</em>
          </h1>
          <p>
            A shared SOL pool on Solana. Stake, register the wallets you want covered, and if one is drained
            through phishing, a bad approval or a stolen key, the payout comes back to you in SOL. While it
            waits, the pool's SOL earns staking yield through Marinade.
          </p>
        </div>
      </section>

      <section className="dapp">
        <div className="wrap">
          <div className="dapp-shell">
            <div className="tabs">
              {TABS.map((t) => (
                <button key={t.id} className={`tab${tab === t.id ? " active" : ""}`} onClick={() => setTab(t.id)}>
                  {t.label}
                </button>
              ))}
            </div>
            {tab === "stake" ? <StakePanel /> : null}
            {tab === "back" ? <BackPanel /> : null}
            {tab === "file" ? (
              <div className="panel">
                <ClaimFilePanel />
                <div />
              </div>
            ) : null}
            {tab === "collect" ? <ClaimsPanel /> : null}
          </div>
        </div>
      </section>

      <section className="numbers">
        <div className="wrap">
          <h2>The pool</h2>
          <div className="sub">Live numbers, read straight from the pool on {NET}.</div>
          <div className="stat-grid">
            <div className="stat-card">
              <div className="v">{pool ? fmtSol(pool.total_staked, 2) : "--"}</div>
              <div className="k">SOL staked</div>
            </div>
            <div className="stat-card">
              <div className="v">{pool ? pool.total_stakers.toString() : "--"}</div>
              <div className="k">Stakers</div>
            </div>
            <div className="stat-card">
              <div className="v">{pool ? fmtSol(pool.total_backed, 2) : "--"}</div>
              <div className="k">SOL backing</div>
            </div>
            <div className="stat-card">
              <div className="v">{pool ? fmtSol(stakeBounds(pool).max, 2) : "--"}</div>
              <div className="k">Max stake (SOL)</div>
            </div>
          </div>
        </div>
      </section>

      <SiteFooter />
    </>
  );
}
