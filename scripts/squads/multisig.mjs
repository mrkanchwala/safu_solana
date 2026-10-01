// The pool's Squads v4 multisig: becomes the program's upgrade authority after `initialize`.
// Members: the pool admin and the co-signer (AWS KMS), all permissions, threshold 2 (set
// 2026-10-01). Creating it needs only the admin's signature; members do not sign creation.
//
//   node multisig.mjs create   --rpc <url> --admin <key.json> --co-signer <pubkey>
//   node multisig.mjs readback --rpc <url> --multisig <pda> --admin <pubkey> --co-signer <pubkey>
//
// Votes (2-of-2): the admin proposes and casts the first vote here; the co-signer's vote is
// `python -m backend.solana_cosigner approve-proposal` on the server (KMS key), which signs only
// when given the fingerprint printed here. Then the admin executes.
//   node multisig.mjs propose-transfer --rpc <url> --multisig <pda> --admin <key.json> --to <pubkey> --lamports <n>
//   node multisig.mjs propose-upgrade  --rpc <url> --multisig <pda> --admin <key.json> --program <id> --buffer <pubkey>
//   node multisig.mjs status  --rpc <url> --multisig <pda> --index <n>
//   node multisig.mjs execute --rpc <url> --multisig <pda> --admin <key.json> --index <n>
//
// Fingerprint = sha256 of the vault transaction account: what is voted on, byte for byte.
//
// Operator tool, run by hand on the operator's machine. Never shipped to the site or the server.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import {
  Connection, Keypair, PublicKey, SystemProgram, SYSVAR_CLOCK_PUBKEY, SYSVAR_RENT_PUBKEY,
  TransactionInstruction, TransactionMessage,
} from "@solana/web3.js";
import * as multisig from "@sqds/multisig";

const { Permissions } = multisig.types;
const THRESHOLD = 2;

function arg(name) {
  const i = process.argv.indexOf(`--${name}`);
  if (i < 0 || !process.argv[i + 1]) throw new Error(`missing --${name}`);
  return process.argv[i + 1];
}

const out = (o) => console.log(JSON.stringify(o, null, 1));
const keypair = (path) => Keypair.fromSecretKey(Uint8Array.from(JSON.parse(readFileSync(path, "utf8"))));
const BPF_UPGRADEABLE = new PublicKey("BPFLoaderUpgradeab1e11111111111111111111111");

async function create() {
  const connection = new Connection(arg("rpc"), "confirmed");
  const admin = keypair(arg("admin"));
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

async function fingerprint(connection, multisigPda, index) {
  const [txPda] = multisig.getTransactionPda({ multisigPda, index });
  const info = await connection.getAccountInfo(txPda, "confirmed");
  if (!info) throw new Error(`no vault transaction at index ${index}`);
  return createHash("sha256").update(info.data).digest("hex");
}

/** Wrap `instructions` (run by vault 0) into a new vault transaction + proposal; admin votes yes. */
async function propose(instructions, what) {
  const connection = new Connection(arg("rpc"), "confirmed");
  const admin = keypair(arg("admin"));
  const multisigPda = new PublicKey(arg("multisig"));
  const m = await multisig.accounts.Multisig.fromAccountAddress(connection, multisigPda);
  const index = BigInt(m.transactionIndex.toString()) + 1n;
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });
  const { blockhash } = await connection.getLatestBlockhash("confirmed");
  const transactionMessage = new TransactionMessage({ payerKey: vaultPda, recentBlockhash: blockhash, instructions });
  const common = { connection, feePayer: admin, multisigPda, transactionIndex: index };
  const sigs = {};
  sigs.create = await multisig.rpc.vaultTransactionCreate({
    ...common, creator: admin.publicKey, vaultIndex: 0, ephemeralSigners: 0, transactionMessage, memo: what,
  });
  await connection.confirmTransaction(sigs.create, "confirmed");
  sigs.proposal = await multisig.rpc.proposalCreate({ ...common, creator: admin });
  await connection.confirmTransaction(sigs.proposal, "confirmed");
  sigs.adminVote = await multisig.rpc.proposalApprove({ ...common, member: admin });
  await connection.confirmTransaction(sigs.adminVote, "confirmed");
  out({ step: "propose", what, index: index.toString(), fingerprint: await fingerprint(connection, multisigPda, index), signatures: sigs });
}

async function proposeTransfer() {
  const multisigPda = new PublicKey(arg("multisig"));
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });
  const to = new PublicKey(arg("to"));
  const lamports = BigInt(arg("lamports"));
  await propose([SystemProgram.transfer({ fromPubkey: vaultPda, toPubkey: to, lamports })],
    `transfer ${lamports} lamports to ${to.toBase58()}`);
}

/** BPF upgradeable loader `Upgrade` (tag 3). The buffer's authority must already be the vault;
 *  the buffer's rent goes back to the admin (spill). */
async function proposeUpgrade() {
  const multisigPda = new PublicKey(arg("multisig"));
  const [vaultPda] = multisig.getVaultPda({ multisigPda, index: 0 });
  const program = new PublicKey(arg("program"));
  const buffer = new PublicKey(arg("buffer"));
  const spill = keypair(arg("admin")).publicKey;
  const [programData] = PublicKey.findProgramAddressSync([program.toBytes()], BPF_UPGRADEABLE);
  const data = Buffer.alloc(4);
  data.writeUInt32LE(3);
  const ix = new TransactionInstruction({
    programId: BPF_UPGRADEABLE,
    keys: [
      { pubkey: programData, isSigner: false, isWritable: true },
      { pubkey: program, isSigner: false, isWritable: true },
      { pubkey: buffer, isSigner: false, isWritable: true },
      { pubkey: spill, isSigner: false, isWritable: true },
      { pubkey: SYSVAR_RENT_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isSigner: false, isWritable: false },
      { pubkey: vaultPda, isSigner: true, isWritable: false },
    ],
    data,
  });
  await propose([ix], `upgrade ${program.toBase58()} from buffer ${buffer.toBase58()}`);
}

async function status() {
  const connection = new Connection(arg("rpc"), "confirmed");
  const multisigPda = new PublicKey(arg("multisig"));
  const index = BigInt(arg("index"));
  const [txPda] = multisig.getTransactionPda({ multisigPda, index });
  const [proposalPda] = multisig.getProposalPda({ multisigPda, transactionIndex: index });
  const tx = await multisig.accounts.VaultTransaction.fromAccountAddress(connection, txPda);
  const p = await multisig.accounts.Proposal.fromAccountAddress(connection, proposalPda);
  const keys = tx.message.accountKeys.map((k) => k.toBase58());
  out({
    step: "status", index: index.toString(), status: p.status.__kind,
    approved: p.approved.map((k) => k.toBase58()), rejected: p.rejected.map((k) => k.toBase58()),
    fingerprint: await fingerprint(connection, multisigPda, index),
    instructions: tx.message.instructions.map((i) => ({
      program: keys[i.programIdIndex], accounts: [...i.accountIndexes].map((a) => keys[a]),
      data: Buffer.from(i.data).toString("hex"),
    })),
  });
}

async function execute() {
  const connection = new Connection(arg("rpc"), "confirmed");
  const admin = keypair(arg("admin"));
  const multisigPda = new PublicKey(arg("multisig"));
  const index = BigInt(arg("index"));
  const signature = await multisig.rpc.vaultTransactionExecute({
    connection, feePayer: admin, multisigPda, transactionIndex: index, member: admin.publicKey,
  });
  await connection.confirmTransaction(signature, "confirmed");
  out({ step: "execute", index: index.toString(), signature });
}

const COMMANDS = {
  create, readback, "propose-transfer": proposeTransfer, "propose-upgrade": proposeUpgrade, status, execute,
};
const cmd = process.argv[2];
await (COMMANDS[cmd] ?? (() => { throw new Error(`command: ${Object.keys(COMMANDS).join(" | ")}`); }))();
