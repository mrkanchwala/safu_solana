// The pool's Squads v4 multisig: becomes the program's upgrade authority after `initialize`.
// Members: the pool admin and the co-signer (AWS KMS), all permissions, threshold 2 (set
// 2026-10-01). Creating it needs only the admin's signature; members do not sign creation.
//
//   node multisig.mjs create   --rpc <url> --admin <key.json> --co-signer <pubkey>
//   node multisig.mjs readback --rpc <url> --multisig <pda> --admin <pubkey> --co-signer <pubkey>
//
// Operator tool, run by hand on the operator's machine. Never shipped to the site or the server.
import { readFileSync } from "node:fs";
import { Connection, Keypair, PublicKey } from "@solana/web3.js";
import * as multisig from "@sqds/multisig";

const { Permissions } = multisig.types;
const THRESHOLD = 2;

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  if (i < 0 || !process.argv[i + 1]) throw new Error(`missing --${name}`);
  return process.argv[i + 1];
}

const out = (o) => console.log(JSON.stringify(o, null, 1));

async function create() {
  const connection = new Connection(arg("rpc"), "confirmed");
  const admin = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(arg("admin"), "utf8"))));
  const coSigner = new PublicKey(arg("co-signer"));
  if (coSigner.equals(admin.publicKey)) throw new Error("co-signer must differ from admin");
  const createKey = Keypair.generate(); // one-time seed for the multisig address, no power after
  const [multisigPda] = multisig.getMultisigPda({ createKey: createKey.publicKey });
  const [programConfigPda] = multisig.getProgramConfigPda({});
  const config = await multisig.accounts.ProgramConfig.fromAccountAddress(connection, programConfigPda);
  const signature = await multisig.rpc.multisigCreateV2({
    connection,
    treasury: config.treasury,
    createKey,
    creator: admin,
    multisigPda,
    configAuthority: null, // autonomous: members change it only by their own 2-of-2 vote
    threshold: THRESHOLD,
    members: [
      { key: admin.publicKey, permissions: Permissions.all() },
      { key: coSigner, permissions: Permissions.all() },
    ],
    timeLock: 0,
    rentCollector: null,
    sendOptions: { skipPreflight: false },
  });
  await connection.confirmTransaction(signature, "confirmed");
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });
  out({ step: "create", signature, multisig: multisigPda.toBase58(), vault: vaultPda.toBase58() });
}

async function readback() {
  const connection = new Connection(arg("rpc"), "confirmed");
  const pda = new PublicKey(arg("multisig"));
  const m = await multisig.accounts.Multisig.fromAccountAddress(connection, pda);
  const members = m.members.map((x) => x.key.toBase58()).sort();
  const want = [arg("admin"), arg("co-signer")].sort();
  const allPerms = m.members.every((x) => x.permissions.mask === Permissions.all().mask);
  const checks = {
    threshold: m.threshold === THRESHOLD,
    members: JSON.stringify(members) === JSON.stringify(want),
    permissions: allPerms,
    configAuthority: m.configAuthority.equals(PublicKey.default),
    timeLock: m.timeLock === 0,
  };
  const [vaultPda] = multisig.getVaultPda({ multisigPda: pda, index: 0 });
  const ok = Object.values(checks).every(Boolean);
  out({ step: "readback", ok, checks, threshold: m.threshold, members, vault: vaultPda.toBase58() });
  process.exit(ok ? 0 : 1);
}

const cmd = process.argv[2];
await ({ create, readback }[cmd] ?? (() => { throw new Error("command: create | readback"); }))();
