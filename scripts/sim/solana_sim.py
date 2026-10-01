"""Solana pool liquidity simulation (2026-09-30).

Same question and method as the multichain study (safu repo, multichain/scripts/sim/solvency_sim.py,
report 2026-09-22): the pool is never meant to repay everyone at once; what matters is liquidity
over time. Withdrawals are paid first, claims stream from what is left and wait for money otherwise.

What is different on Solana (pool-core params.rs defaults):
- stakes 0.1%-1% of the cap (multichain 0.01%-0.1%): fewer, larger stakers
- stakers get 100% of the yield on their capital (STAKER_YIELD_BPS); no protocol buffer by default
- claim payouts per day follow utilisation (open claims / capacity): <20% -> 5%, <50% -> 3%, else 1%
  of capacity (PAYOUT_*_BPS)
- 80% of capacity in mSOL (DEPLOY_BPS), the rest liquid; Marinade yield ~6%/yr
- Marinade's unstake fee: a payout larger than the liquid SOL unstakes the shortfall and the
  payee pays the fee on that part; the pool's own refill of its liquid 20% pays the fee itself
  (a loss, by design, part of the same hidden gap as claims paid beyond a stake)
- gate 90 d, cooldown 7 d, vesting 45 d, tiers A 15x / B 10x / C 5x (same as multichain)

Units: fractions of the pool cap (cap = 1.0), so results read as "% of cap" at any SOL size.
Run: python3 solana_sim.py [runs] > out.txt   (grid, then steady state)
"""

from __future__ import annotations

import itertools
import sys

import numpy as np

CAP = 1.0
MIN_STAKE, MAX_STAKE = 0.001, 0.01
TIER_RATIO = np.array([15.0, 10.0, 5.0])
DAYS = 730
GATE, COOLDOWN, VEST = 90, 7, 45
DEPLOY = 0.80
APY = 0.06
# Loss per drain relative to the cap: multichain used median $400 on a $100K cap.
LOSS_MEDIAN, LOSS_SIGMA = 0.004, 1.6
PAYOUT_BANDS = ((0.20, 0.05), (0.50, 0.03), (9.99, 0.01))
MIXES = {"even": (0.33, 0.33, 0.34), "A-heavy": (0.50, 0.30, 0.20)}


def payout_rate(allocated, capacity):
    util = allocated / capacity if capacity > 0 else 1.0
    for top, rate in PAYOUT_BANDS:
        if util < top:
            return rate
    return 0.01


class Pool:
    """Liquid SOL + mSOL at book; a payout larger than the liquid part unstakes the shortfall."""

    def __init__(self, fee):
        self.liquid, self.deployed, self.fee = 0.0, 0.0, fee
        self.pool_fees, self.payee_fees = 0.0, 0.0

    @property
    def assets(self):
        return self.liquid + self.deployed

    def pay(self, amount):
        """Pays `amount` if the pool holds it; the payee's fee on the unstaked part comes off."""
        if amount > self.assets + 1e-12:
            return False
        short = max(amount - self.liquid, 0.0)
        self.liquid -= amount - short
        self.deployed -= short
        self.payee_fees += short * self.fee
        return True

    def pay_part(self, amount):
        amount = min(amount, self.assets)
        self.pay(amount)
        return amount

    def rebalance(self, capacity):
        target = (1 - DEPLOY) * capacity
        if self.liquid < target and self.deployed > 0:
            move = min(target - self.liquid, self.deployed)
            self.deployed -= move
            self.liquid += move * (1 - self.fee)
            self.pool_fees += move * self.fee
        elif self.liquid > target:
            self.deployed += self.liquid - target
            self.liquid = target


def simulate(hack, mix, inflow, staker_share, fee, rng, panic=False, seed=0.0):
    n_max = 4000
    amount = np.zeros(n_max)
    tier = np.zeros(n_max, dtype=int)
    joined = np.full(n_max, -1)
    active = np.zeros(n_max, dtype=bool)
    idx_at = np.ones(n_max)
    index = 1.0
    pool = Pool(fee)
    pool.liquid = seed
    total = 0.0
    claims = []  # [remaining, start, entitlement]
    waits = []  # [owed, since]
    max_wait = max_delay = 0
    min_gap = 1e9
    paid_out = 0.0

    def add(day):
        nonlocal total
        if total + MAX_STAKE > CAP:
            return
        free = np.flatnonzero(~active & (joined < 0))
        if free.size == 0:
            return
        i = free[0]
        a = rng.uniform(MIN_STAKE, MAX_STAKE)
        amount[i], tier[i], joined[i], active[i], idx_at[i] = a, rng.choice(3, p=mix), day, True, index
        pool.liquid += a
        total += a

    while total < 0.6 * CAP:
        add(0)
    pool.rebalance(total + seed)

    for day in range(DAYS):
        y = pool.deployed * APY / 365
        pool.deployed += y
        if total > 0:
            index += y * staker_share * (total / max(total + seed, 1e-12)) / total

        in_panic = panic and 360 <= day < 420
        mean_new = inflow * CAP / ((MIN_STAKE + MAX_STAKE) / 2) / 30
        for _ in range(0 if in_panic else rng.poisson(mean_new)):
            add(day)

        act = np.flatnonzero(active)
        for i in act[rng.random(act.size) < hack / 365]:
            ent = min(rng.lognormal(np.log(LOSS_MEDIAN), LOSS_SIGMA), amount[i] * TIER_RATIO[tier[i]])
            claims.append([ent, max(day, joined[i] + GATE) + COOLDOWN, ent])
            total -= amount[i]
            active[i] = False
            joined[i] = -2

        act = np.flatnonzero(active)
        p_out = np.where(day - joined[act] < 90, 0.15, 0.04) / 30 * (3 if in_panic else 1)
        for i in act[rng.random(act.size) < p_out]:
            active[i] = False
            joined[i] = -3
            total -= amount[i]
            waits.append([amount[i] * index / idx_at[i], day])
        left = []
        for w in waits:
            if pool.pay(w[0]):
                paid_out += w[0]
                max_wait = max(max_wait, day - w[1])
            else:
                left.append(w)
        waits = left
        if waits:
            max_wait = max(max_wait, max(day - w[1] for w in waits))

        capacity = total + seed
        open_claims = sum(c[0] for c in claims)
        budget = payout_rate(open_claims, capacity) * max(capacity, 1e-9)
        for c in claims:
            if day < c[1]:
                continue
            sched = c[2] * min(day - c[1] + 1, VEST) / VEST
            due = max(sched - (c[2] - c[0]), 0.0)
            pay = pool.pay_part(min(due, budget))
            c[0] -= pay
            budget -= pay
            paid_out += pay
            if c[2] - c[0] < sched - 1e-9 and day > c[1] + VEST:
                max_delay = max(max_delay, day - (c[1] + VEST))
        claims = [c for c in claims if c[0] > 1e-12]
        pool.rebalance(capacity)

        owed = float(np.sum(amount[active] * index / idx_at[active])) + sum(w[0] for w in waits)
        min_gap = min(min_gap, pool.assets - owed - sum(c[0] for c in claims) - seed)

    return dict(
        wait=max_wait,
        delay=max_delay,
        unpaid_claims=sum(c[0] for c in claims),
        unpaid_withdrawals=sum(w[0] for w in waits),
        gap=min_gap,
        pool_fees=pool.pool_fees,
        payee_fee_pct=100 * pool.payee_fees / max(paid_out, 1e-12),
    )


def grid(runs):
    print("mix|hack%/yr|inflow %cap/mo|panic|fee%|staker share%|max withdraw wait d|max claim delay d|"
          "unpaid claims end %cap|unpaid withdrawals end %cap|worst gap %cap|pool fees %cap|payee fee % of payouts")
    for (mn, mix), h, f, pn, fee, share in itertools.product(
        MIXES.items(), [0.0025, 0.005, 0.01, 0.02, 0.03], [0.0, 0.03, 0.10], [False, True], [0.003, 0.03],
        [1.0, 0.5],
    ):
        rng = np.random.default_rng(42)
        rs = [simulate(h, mix, f, share, fee, rng, panic=pn) for _ in range(runs)]
        print(f"{mn}|{h*100:.2f}|{f*100:.0f}|{'yes' if pn else 'no'}|{fee*100:.1f}|{share*100:.0f}|"
              f"{max(r['wait'] for r in rs)}|{max(r['delay'] for r in rs)}|"
              f"{100*max(r['unpaid_claims'] for r in rs):.2f}|{100*max(r['unpaid_withdrawals'] for r in rs):.2f}|"
              f"{100*min(r['gap'] for r in rs):.2f}|{100*max(r['pool_fees'] for r in rs):.2f}|"
              f"{max(r['payee_fee_pct'] for r in rs):.2f}")
        sys.stdout.flush()


def steady(runs, fee, share):
    """Constant pool size: every leaver and claimant replaced. First day cash runs short."""
    print(f"\nsteady state, fee {fee*100:.1f}%, staker share {share*100:.0f}%")
    print("mix|hack %/yr|runs failing within 10y|median first failure (years)|earliest|first to fail")
    for mn, mix in MIXES.items():
        for h in [0.01, 0.03, 0.05, 0.10, 0.20]:
            rng = np.random.default_rng(7)
            res = [first_failure(h, mix, fee, share, rng) for _ in range(runs)]
            days = [d for d, _ in res if d is not None]
            kinds = sorted({k for d, k in res if d is not None})
            med = f"{np.median(days)/365:.1f}" if days else ">10"
            early = f"{min(days)/365:.1f}" if days else ">10"
            print(f"{mn}|{h*100:.0f}|{len(days)}/{runs}|{med}|{early}|{','.join(kinds) or '-'}")
            sys.stdout.flush()


def first_failure(hack, mix, fee, share, rng, fill=0.6):
    n = int(fill * CAP / ((MIN_STAKE + MAX_STAKE) / 2))
    amount = rng.uniform(MIN_STAKE, MAX_STAKE, n)
    tier = rng.choice(3, size=n, p=mix)
    joined = np.zeros(n, dtype=int)
    idx_at = np.ones(n)
    index = 1.0
    pool = Pool(fee)
    pool.liquid = float(amount.sum())
    pool.rebalance(pool.liquid)
    claims = []

    def replace(i, day):
        amount[i] = rng.uniform(MIN_STAKE, MAX_STAKE)
        tier[i] = rng.choice(3, p=mix)
        joined[i], idx_at[i] = day, index
        pool.liquid += amount[i]

    for day in range(10 * 365):
        total = float(amount.sum())
        y = pool.deployed * APY / 365
        pool.deployed += y
        index += y * share / total
        for i in np.flatnonzero(rng.random(n) < hack / 365):
            ent = min(rng.lognormal(np.log(LOSS_MEDIAN), LOSS_SIGMA), amount[i] * TIER_RATIO[tier[i]])
            claims.append([ent, max(day, joined[i] + GATE) + COOLDOWN, ent])
            replace(i, day)
        leave = np.flatnonzero(rng.random(n) < np.where(day - joined < 90, 0.15, 0.04) / 30)
        for i in leave:
            if not pool.pay(amount[i] * index / idx_at[i]):
                return day, "withdrawal"
            replace(i, day)
        total = float(amount.sum())
        budget = payout_rate(sum(c[0] for c in claims), total) * total
        for c in claims:
            if day < c[1]:
                continue
            sched = c[2] * min(day - c[1] + 1, VEST) / VEST
            due = max(sched - (c[2] - c[0]), 0.0)
            pay = pool.pay_part(min(due, budget))
            c[0] -= pay
            budget -= pay
            if c[2] - c[0] < sched - 1e-9 and day > c[1] + VEST:
                return day, "claim"
        claims = [c for c in claims if c[0] > 1e-12]
        pool.rebalance(total)
    return None, "none"


if __name__ == "__main__":
    runs = int(sys.argv[1]) if len(sys.argv) > 1 else 8
    grid(runs)
    for fee, share in ((0.003, 1.0), (0.03, 1.0), (0.003, 0.5)):
        steady(5, fee, share)
