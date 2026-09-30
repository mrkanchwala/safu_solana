import { describe, expect, it } from "vitest";

import { toFriendlyError } from "./friendly-error";

describe("toFriendlyError", () => {
  it("matches a known backend rejection reason", () => {
    const result = toFriendlyError(new Error("WALLET_NOT_COVERED: wallet not registered"));
    expect(result.message).toContain("isn't one of your covered wallets");
    expect(result.raw).toContain("WALLET_NOT_COVERED");
  });

  it("explains the staking-wallet refusal", () => {
    const r = toFriendlyError(new Error("STAKER_WALLET: a covered wallet can't be the same as your staking wallet."));
    expect(r.message).toContain("separate wallet");
  });

  it("matches a wallet-rejection message case-insensitively", () => {
    expect(toFriendlyError(new Error("User Rejected the request")).message).toBe("Request was declined in the wallet.");
  });

  it("falls back to a generic message for an unmatched reason, never leaking the raw string", () => {
    const result = toFriendlyError(new Error("SOME_UNMAPPED_CODE"));
    expect(result.message).toContain("didn't go through");
    expect(result.message).not.toContain("SOME_UNMAPPED_CODE");
  });

  it("treats WalletConnect's closed-modal rejection (a plain object) as a cancel", () => {
    expect(toFriendlyError({ code: -1, message: "The user closed the modal." }).cancelled).toBe(true);
  });

  it("explains WalletConnect's unsupported-chains rejection", () => {
    expect(toFriendlyError({ code: 5100, message: "Unsupported chains." }).message).toContain("test network");
  });

  it("shows the app's own plain messages as written", () => {
    expect(toFriendlyError(new Error("Connect a wallet first.")).message).toBe("Connect a wallet first.");
  });

  it("translates a program error by its IDL name", () => {
    expect(toFriendlyError(new Error("PROGRAM_ERROR:NothingVested")).message).toContain("Nothing new to collect");
    expect(toFriendlyError(new Error("PROGRAM_ERROR:StakeOutOfRange")).message).toContain("stake limits");
  });

  it("never shows an unmapped program error name", () => {
    const r = toFriendlyError(new Error("PROGRAM_ERROR:NotOracle"));
    expect(r.message).not.toMatch(/NotOracle|PROGRAM/);
  });

  it("explains a Marinade refusal without naming a pool rule", () => {
    const r = toFriendlyError(new Error("MARINADE_REFUSED: Program MarBmsSgKXdrN1egZf5sqe1TMai9K1rChYNDJgjq7aD failed: custom program error: 0x178e"));
    expect(r.message).toContain("Marinade");
    expect(r.message).not.toMatch(/0x|MARINADE_/);
  });

  it("explains missing SOL", () => {
    expect(toFriendlyError(new Error("Attempt to debit an account but found no record of a prior credit.")).message).toContain("SOL");
  });

  it("shows no reason code in any backend message", () => {
    for (const code of ["CHAIN_UNAVAILABLE: x", "BROADCAST_FAILED: x", "PRICE_UNAVAILABLE: x", "TooManyWallets: x"]) {
      expect(toFriendlyError(new Error(code)).message).not.toMatch(/[A-Z]{3,}_[A-Z]|TooMany/);
    }
  });
});
