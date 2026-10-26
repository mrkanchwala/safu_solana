#!/usr/bin/env python3
"""Every hand-typed Solana address must exist on devnet or mainnet (a typo exists on neither).

    python3 scripts/check_builtin_ids.py DIR_OR_FILE...

Built-in ids (System, Token, Associated Token, Squads, ...) are typed once and marked `hardcode-ok`
or written with `pubkey!(...)`. Tests that mock the chain or run on LiteSVM pass with a typo in one
of them, so a wrong id only shows up on a real cluster (it happened: a mistyped Associated Token
program id reached the claim API, 2026-10-26). This asks both public clusters whether each address
is a real account; test fixtures may live on one cluster only. No dependencies; network only.
"""

from __future__ import annotations

import json
import re
import sys
import time
import urllib.request
from pathlib import Path

CLUSTERS = {"devnet": "https://api.devnet.solana.com", "mainnet": "https://api.mainnet-beta.solana.com"}
# Real ids that are not stored as accounts, so a lookup cannot find them.
NOT_ACCOUNTS = {
    "Sysvar1nstructions1111111111111111111111111": "instructions sysvar: built per transaction",
    "nu11111111111111111111111111111111111111111": "Kamino Lend's 'no account' placeholder",
}
MARKED = re.compile(r"hardcode-ok|pubkey!\(")
ADDRESS = re.compile(r"[\"']([1-9A-HJ-NP-Za-km-z]{32,44})[\"']")
SUFFIXES = {".py", ".ts", ".tsx", ".rs"}
SKIP = {"node_modules", "target", ".venv", "venv", ".git", "dist", ".next"}


def files(paths: list[str]):
    for p in map(Path, paths):
        if p.is_file():
            yield p
        else:
            for f in p.rglob("*"):
                if f.suffix in SUFFIXES and f.is_file() and not SKIP & set(f.parts):
                    yield f


def found(paths: list[str]) -> dict[str, list[str]]:
    out: dict[str, list[str]] = {}
    for f in files(paths):
        for n, line in enumerate(f.read_text(errors="ignore").splitlines(), 1):
            if MARKED.search(line):
                for address in ADDRESS.findall(line):
                    out.setdefault(address, []).append(f"{f}:{n}")
    return out


def exists(url: str, addresses: list[str]) -> dict[str, bool]:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getMultipleAccounts",
                       "params": [addresses, {"encoding": "base64", "dataSlice": {"offset": 0, "length": 0}}]})
    for attempt in range(4):
        try:
            req = urllib.request.Request(url, body.encode(), {"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=30) as r:
                values = json.load(r)["result"]["value"]
            return {a: v is not None for a, v in zip(addresses, values)}
        except Exception as exc:  # noqa: BLE001 - network trouble is retried, then reported
            if attempt == 3:
                raise SystemExit(f"{url}: {exc}")
            time.sleep(5 * (attempt + 1))
    raise AssertionError


def main() -> int:
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    addresses = found(sys.argv[1:])
    check = sorted(a for a in addresses if a not in NOT_ACCOUNTS)
    seen: dict[str, list[str]] = {a: [] for a in check}
    for name, url in CLUSTERS.items():
        for i in range(0, len(check), 100):
            for a, ok in exists(url, check[i:i + 100]).items():
                if ok:
                    seen[a].append(name)
    bad = [a for a in check if not seen[a]]
    for a in bad:
        print(f"NOT ON ANY CLUSTER: {a} at {', '.join(addresses[a])}")
    print(f"{len(addresses)} hand-typed addresses, {len(check)} looked up on {', '.join(CLUSTERS)}: "
          f"{'all exist' if not bad else f'{len(bad)} missing'}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
