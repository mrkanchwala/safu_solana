//! CCTP against Circle's real programs: devnet MessageTransmitterV2 and TokenMessengerMinterV2
//! (`fixtures/cctp_*.so`) with their state as dumped 2026-10-22, and the pool on Circle's devnet
//! USDC. The one change: Circle's two attester keys are swapped for two test keys, so the tests can
//! attest their own burns. Everything else (nonces, token pairs, minting, burning) is Circle's code.
//!
//! Owners sign as their wallets would: an Ethereum key with `personal_sign`, a Stellar key with
//! SEP-53 `signMessage` (here a Solana keypair: both are plain Ed25519).
mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use base64::Engine;
use common::*;
use k256::ecdsa::SigningKey;
use pool_core::cctp as c;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::instructions::OwnerSignature;
use safu_pool::state::{CctpAccount, Pool};
use sha3::{Digest, Keccak256};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

const MT: Pubkey = safu_pool::constants::CCTP_MESSAGE_TRANSMITTER_ID;
const TMM: Pubkey = safu_pool::constants::CCTP_TOKEN_MESSENGER_MINTER_ID;
const CIRCLE_USDC: Pubkey =
    anchor_lang::prelude::pubkey!("4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU");
/// Sepolia USDC (the Ethereum token pair) and Stellar testnet USDC (the Stellar one).
const SEPOLIA_USDC: &str = "1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";
const STELLAR_USDC: &str = "CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA";
/// `MessageTransmitter`: discriminator, 4 keys, paused, local domain, version, threshold, then the
/// attesters vector (u32 length, 32-byte entries holding a left-padded Ethereum address).
const ATTESTERS_AT: usize = 8 + 4 * 32 + 1 + 4 + 4 + 4 + 4;
/// `RemoteTokenMessenger`: discriminator, domain, then the remote messenger's address.
const REMOTE_MESSENGER_AT: usize = 8 + 4;
/// `finality_threshold_executed` for a finalized burn.
const FINALIZED: u32 = 2000;

fn fixture(name: &str) -> Account {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let j: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let a = &j["account"];
    Account {
        lamports: a["lamports"].as_u64().unwrap(),
        data: base64::engine::general_purpose::STANDARD
            .decode(a["data"][0].as_str().unwrap())
            .unwrap(),
        owner: a["owner"].as_str().unwrap().parse().unwrap(),
        executable: false,
        rent_epoch: 0,
    }
}

fn mt_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &MT).0
}
fn tmm_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &TMM).0
}

fn padded(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..20 {
        out[12 + i] = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

fn burn_token(domain: u32) -> [u8; 32] {
    if domain == c::domain::ETHEREUM {
        padded(SEPOLIA_USDC)
    } else {
        strkey_payload(STELLAR_USDC)
    }
}

fn keccak(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Keccak256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// A fresh secp256k1 key (entropy from a new Solana keypair's secret).
fn random_eth_key() -> SigningKey {
    SigningKey::from_slice(&Keypair::new().to_bytes()[..32]).unwrap()
}

fn eth_address(key: &SigningKey) -> [u8; 20] {
    let point = key.verifying_key().to_encoded_point(false);
    keccak(&[&point.as_bytes()[1..]])[12..].try_into().unwrap()
}

/// r || s || v (27/28) over a 32-byte hash.
fn eth_sign(key: &SigningKey, hash: &[u8; 32]) -> [u8; 65] {
    let (sig, rec) = key.sign_prehash_recoverable(hash).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = rec.to_byte() + 27;
    out
}

/// A home-chain owner: an Ethereum key or a Stellar (Ed25519) key.
enum HomeKey {
    Eth(SigningKey),
    Stellar(Keypair),
}

impl HomeKey {
    fn eth() -> HomeKey {
        HomeKey::Eth(random_eth_key())
    }
    fn stellar() -> HomeKey {
        HomeKey::Stellar(Keypair::new())
    }
    fn domain(&self) -> u32 {
        match self {
            HomeKey::Eth(_) => c::domain::ETHEREUM,
            HomeKey::Stellar(_) => c::domain::STELLAR,
        }
    }
    fn bytes32(&self) -> [u8; 32] {
        match self {
            HomeKey::Eth(k) => {
                let mut out = [0u8; 32];
                out[12..].copy_from_slice(&eth_address(k));
                out
            }
            HomeKey::Stellar(k) => k.pubkey().to_bytes(),
        }
    }
}

struct Cctp {
    env: Env,
    attesters: Vec<SigningKey>,
    relayer: Keypair,
    nonces: u64,
}

/// Instruction accounts as the program sees them: a key is a signer if any instruction signs it or
/// it pays the fee, writable if any instruction writes it.
fn seen(tx: &[Instruction], payer: &Pubkey, metas: &[AccountMeta]) -> Vec<AccountMeta> {
    metas
        .iter()
        .map(|m| {
            let all = tx
                .iter()
                .flat_map(|i| i.accounts.iter())
                .filter(|a| a.pubkey == m.pubkey);
            let (s, w) = all.fold((m.pubkey == *payer, m.pubkey == *payer), |(s, w), a| {
                (s || a.is_signer, w || a.is_writable)
            });
            AccountMeta {
                pubkey: m.pubkey,
                is_signer: s,
                is_writable: w,
            }
        })
        .collect()
}

/// `instructions::cctp::call_hash` over metas.
fn call_hash(data: &[u8], metas: &[AccountMeta]) -> [u8; 32] {
    let mut parts: Vec<Vec<u8>> = vec![data.to_vec()];
    for m in metas {
        let mut p = m.pubkey.to_bytes().to_vec();
        p.push(u8::from(m.is_writable));
        p.push(u8::from(m.is_signer));
        parts.push(p);
    }
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    solana_sha256_hasher::hashv(&refs).to_bytes()
}

impl Cctp {
    fn new() -> Cctp {
        let mut env = Env::new_at(CIRCLE_USDC, START);
        env.svm
            .add_program(MT, include_bytes!("fixtures/cctp_message_transmitter.so"))
            .unwrap();
        env.svm
            .add_program(
                TMM,
                include_bytes!("fixtures/cctp_token_messenger_minter.so"),
            )
            .unwrap();
        let fee_recipient = {
            let tm = fixture("cctp_token_messenger");
            Pubkey::try_from(&tm.data[109..141]).unwrap()
        };
        for (key, name) in [
            (
                mt_pda(&[b"message_transmitter"]),
                "cctp_message_transmitter",
            ),
            (tmm_pda(&[b"token_messenger"]), "cctp_token_messenger"),
            (tmm_pda(&[b"token_minter"]), "cctp_token_minter"),
            (
                tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                "cctp_local_token",
            ),
            (
                tmm_pda(&[b"remote_token_messenger", b"0"]),
                "cctp_remote_messenger_0",
            ),
            (
                tmm_pda(&[b"remote_token_messenger", b"27"]),
                "cctp_remote_messenger_27",
            ),
            (tmm_pda(&[b"custody", CIRCLE_USDC.as_ref()]), "cctp_custody"),
            (
                tmm_pda(&[b"token_pair", b"0", &burn_token(c::domain::ETHEREUM)]),
                "cctp_token_pair_0",
            ),
            (
                tmm_pda(&[b"token_pair", b"27", &burn_token(c::domain::STELLAR)]),
                "cctp_token_pair_27",
            ),
            (CIRCLE_USDC, "cctp_usdc_mint"),
            (
                Pubkey::find_program_address(
                    &[fee_recipient.as_ref(), TOKEN.as_ref(), CIRCLE_USDC.as_ref()],
                    &ATA_PROGRAM,
                )
                .0,
                "cctp_fee_recipient_ata",
            ),
        ] {
            env.svm.set_account(key, fixture(name)).unwrap();
        }
        // Two test attesters in place of Circle's, in increasing address order as Circle requires.
        let mut attesters: Vec<SigningKey> = (0..2).map(|_| random_eth_key()).collect();
        attesters.sort_by_key(eth_address);
        let state = mt_pda(&[b"message_transmitter"]);
        let mut a = env.svm.get_account(&state).unwrap();
        assert_eq!(&a.data[ATTESTERS_AT - 4..ATTESTERS_AT], &2u32.to_le_bytes());
        for (i, k) in attesters.iter().enumerate() {
            let at = ATTESTERS_AT + 32 * i;
            a.data[at..at + 12].fill(0);
            a.data[at + 12..at + 32].copy_from_slice(&eth_address(k));
        }
        env.svm.set_account(state, a).unwrap();

        let relayer = Keypair::new();
        env.svm.airdrop(&relayer.pubkey(), 100 * SOL).unwrap();
        let mut t = Cctp {
            env,
            attesters,
            relayer,
            nonces: 0,
        };
        for mode in [c::MODE_STAKE, c::MODE_BACK] {
            let ix = t.open_inbox_ix(mode);
            let r = t.relayer.insecure_clone();
            t.env.ok(&[ix], &[&r]);
        }
        t
    }

    fn pool(&self) -> Pubkey {
        self.env.pool()
    }
    fn authority(&self) -> Pubkey {
        pda(&[SEED_CCTP_AUTHORITY, self.pool().as_ref()])
    }
    fn inbox(&self, mode: u8) -> Pubkey {
        pda(&[SEED_CCTP_INBOX, self.pool().as_ref(), &[mode]])
    }
    fn identity(&self, mode: u8, owner: &HomeKey) -> Pubkey {
        pda(&[
            SEED_CCTP_IDENTITY,
            self.pool().as_ref(),
            &[mode],
            &owner.domain().to_be_bytes(),
            &owner.bytes32(),
        ])
    }
    fn account(&self, identity: &Pubkey) -> Pubkey {
        pda(&[SEED_CCTP_ACCOUNT, identity.as_ref()])
    }
    fn held(&self, identity: &Pubkey) -> Pubkey {
        pda(&[SEED_CCTP_HELD, identity.as_ref()])
    }
    fn state(&self, identity: &Pubkey) -> CctpAccount {
        self.env.read(&self.account(identity))
    }

    fn open_inbox_ix(&self, mode: u8) -> Instruction {
        self.env.ix(
            safu_pool::accounts::CctpOpenInbox {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                authority: self.authority(),
                inbox: self.inbox(mode),
                usdc_mint: CIRCLE_USDC,
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            safu_pool::instruction::CctpOpenInbox { mode },
        )
    }

    /// A finalized v2 burn from `owner`'s chain to this pool's `mode` inbox.
    fn burn(&mut self, owner: &HomeKey, mode: u8, amount: u64) -> Vec<u8> {
        let (caller, recipient) = (self.authority().to_bytes(), self.inbox(mode).to_bytes());
        self.burn_to(owner, owner.domain(), amount, caller, recipient)
    }

    fn burn_to(
        &mut self,
        owner: &HomeKey,
        source: u32,
        amount: u64,
        destination_caller: [u8; 32],
        mint_recipient: [u8; 32],
    ) -> Vec<u8> {
        self.nonces += 1;
        let mut nonce = [0u8; 32];
        nonce[24..].copy_from_slice(&self.nonces.to_be_bytes());
        // Circle's messenger on the source chain (none for Solana, which is not a home chain).
        let sender: [u8; 32] = if source == c::domain::SOLANA {
            [0; 32]
        } else {
            let remote = fixture(&format!("cctp_remote_messenger_{source}"));
            read_32(&remote.data, REMOTE_MESSENGER_AT)
        };
        let mut m = Vec::new();
        m.extend_from_slice(&c::MESSAGE_VERSION.to_be_bytes());
        m.extend_from_slice(&source.to_be_bytes());
        m.extend_from_slice(&c::domain::SOLANA.to_be_bytes());
        m.extend_from_slice(&nonce);
        m.extend_from_slice(&sender);
        m.extend_from_slice(TMM.as_ref());
        m.extend_from_slice(&destination_caller);
        m.extend_from_slice(&FINALIZED.to_be_bytes());
        m.extend_from_slice(&FINALIZED.to_be_bytes());
        assert_eq!(m.len(), c::header::BODY);
        let mut amount32 = [0u8; 32];
        amount32[24..].copy_from_slice(&amount.to_be_bytes());
        m.extend_from_slice(&c::BURN_MESSAGE_VERSION.to_be_bytes());
        m.extend_from_slice(&burn_token(source));
        m.extend_from_slice(&mint_recipient);
        m.extend_from_slice(&amount32);
        m.extend_from_slice(&owner.bytes32());
        m.extend_from_slice(&[0u8; 32]); // max fee
        m.extend_from_slice(&[0u8; 32]); // fee executed
        m.extend_from_slice(&[0u8; 32]); // expiration block
        m
    }

    fn attest(&self, message: &[u8]) -> Vec<u8> {
        let hash = keccak(&[message]);
        self.attesters
            .iter()
            .flat_map(|k| eth_sign(k, &hash))
            .collect()
    }

    fn receive_ix(&self, mode: u8, owner: &HomeKey, message: Vec<u8>) -> Instruction {
        let burn = c::parse_inbound(&message).unwrap();
        let identity = self.identity(mode, owner);
        let fee_recipient =
            Pubkey::try_from(&fixture("cctp_token_messenger").data[109..141]).unwrap();
        let attestation = self.attest(&message);
        let source = burn.source_domain.to_string();
        self.env.ix(
            safu_pool::accounts::CctpReceive {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                authority: self.authority(),
                inbox: self.inbox(mode),
                identity,
                account: self.account(&identity),
                held: self.held(&identity),
                usdc_mint: CIRCLE_USDC,
                token_program: TOKEN,
                system_program: SYSTEM,
                circle: safu_pool::accounts::CircleReceive {
                    message_transmitter_program: MT,
                    token_messenger_minter_program: TMM,
                    message_transmitter_authority: mt_pda(&[
                        b"message_transmitter_authority",
                        TMM.as_ref(),
                    ]),
                    message_transmitter: mt_pda(&[b"message_transmitter"]),
                    used_nonce: mt_pda(&[b"used_nonce", &burn.nonce]),
                    message_transmitter_event_authority: mt_pda(&[b"__event_authority"]),
                    token_messenger: tmm_pda(&[b"token_messenger"]),
                    remote_token_messenger: tmm_pda(&[
                        b"remote_token_messenger",
                        source.as_bytes(),
                    ]),
                    token_minter: tmm_pda(&[b"token_minter"]),
                    local_token: tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                    token_pair: tmm_pda(&[b"token_pair", source.as_bytes(), &burn.burn_token]),
                    fee_recipient_token_account: Pubkey::find_program_address(
                        &[fee_recipient.as_ref(), TOKEN.as_ref(), CIRCLE_USDC.as_ref()],
                        &ATA_PROGRAM,
                    )
                    .0,
                    custody: tmm_pda(&[b"custody", CIRCLE_USDC.as_ref()]),
                    token_messenger_event_authority: tmm_pda(&[b"__event_authority"]),
                },
            },
            safu_pool::instruction::CctpReceive {
                mode,
                home_domain: owner.domain(),
                home_owner: owner.bytes32(),
                message,
                attestation,
            },
        )
    }

    fn receive(&mut self, mode: u8, owner: &HomeKey, amount: u64) -> Pubkey {
        let m = self.burn(owner, mode, amount);
        let ix = self.receive_ix(mode, owner, m);
        let r = self.relayer.insecure_clone();
        self.env.ok(&[ix], &[&r]);
        self.identity(mode, owner)
    }

    fn call_accounts(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        safu_pool::accounts::CctpCall {
            account: self.account(identity),
            identity: *identity,
            pool: self.pool(),
            instructions: IX_SYSVAR,
            program: safu_pool::ID,
        }
        .to_account_metas(None)
    }

    /// A pool call's accounts with the identity in them as the program will sign for it: not a
    /// transaction signer.
    fn as_identity(metas: Vec<AccountMeta>, identity: &Pubkey) -> Vec<AccountMeta> {
        metas
            .into_iter()
            .map(|mut m| {
                if m.pubkey == *identity {
                    m.is_signer = false;
                }
                m
            })
            .collect()
    }

    fn stake_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Stake {
            payer: self.relayer.pubkey(),
            staker: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            stake_record: self.env.stake_record(identity),
            staker_usdc: self.held(identity),
            backer_record: None,
            system_program: SYSTEM,
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    fn back_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Back {
            payer: self.relayer.pubkey(),
            backer: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            backer_record: self.env.backer_record(identity),
            backer_usdc: self.held(identity),
            system_program: SYSTEM,
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    fn withdraw_metas(&self, identity: &Pubkey) -> Vec<AccountMeta> {
        let accounts = safu_pool::accounts::Withdraw {
            staker: *identity,
            pool: self.pool(),
            vault: self.env.vault(),
            stake_record: self.env.stake_record(identity),
            beneficiary_usdc: self.held(identity),
            cash: self.env.cash(),
        };
        Self::as_identity(accounts.to_account_metas(None), identity)
    }

    fn deploy_ix(&self, identity: &Pubkey, mode: u8) -> Instruction {
        let mut accounts = self.call_accounts(identity);
        accounts.extend(if mode == c::MODE_STAKE {
            self.stake_metas(identity)
        } else {
            self.back_metas(identity)
        });
        Instruction {
            program_id: safu_pool::ID,
            accounts,
            data: safu_pool::instruction::CctpDeploy {}.data(),
        }
    }

    fn deploy(&mut self, identity: &Pubkey, mode: u8) -> Result<(), String> {
        let ix = self.deploy_ix(identity, mode);
        let r = self.relayer.insecure_clone();
        self.env.send(&[ix], &[&r])
    }

    /// The owner's signed `cctp_act` for `data` over `inner`, with the precompile instruction first
    /// for a Stellar owner. `tamper` edits the instruction after signing.
    fn act_tx(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        inner: Vec<AccountMeta>,
        data: Vec<u8>,
        nonce: u64,
        expires_at: i64,
    ) -> Vec<Instruction> {
        let mut accounts = self.call_accounts(identity);
        let signed_accounts = inner.len() as u8;
        accounts.extend(inner.iter().cloned());
        let mut act = Instruction {
            program_id: safu_pool::ID,
            accounts,
            data: vec![],
        };
        let tx_view = [act.clone()];
        let signed = seen(&tx_view, &self.relayer.pubkey(), &inner);
        let hash = call_hash(&data, &signed);
        let (signature, pre) = self.sign(owner, identity, nonce, expires_at, &hash);
        act.data = safu_pool::instruction::CctpAct {
            nonce,
            expires_at,
            signature,
            signed_accounts,
            data,
        }
        .data();
        pre.into_iter().chain([act]).collect()
    }

    /// The owner's signature over an action, and the precompile instruction a Stellar one needs.
    fn sign(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        nonce: u64,
        expires_at: i64,
        call_hash: &[u8; 32],
    ) -> (OwnerSignature, Option<Instruction>) {
        let preimage = c::action_preimage(
            &safu_pool::ID.to_bytes(),
            &self.account(identity).to_bytes(),
            nonce,
            expires_at,
            call_hash,
        );
        let digest = solana_sha256_hasher::hash(&preimage).to_bytes();
        match owner {
            HomeKey::Eth(k) => {
                let hash = keccak(&[c::EIP191_PREFIX_32, &digest]);
                (OwnerSignature::Evm(eth_sign(k, &hash)), None)
            }
            HomeKey::Stellar(k) => {
                let text: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                let message =
                    solana_sha256_hasher::hashv(&[c::SEP53_PREFIX, text.as_bytes()]).to_bytes();
                (
                    OwnerSignature::Stellar,
                    Some(ed25519_ix(k, &message, [0, 0, 0])),
                )
            }
        }
    }

    fn act(
        &mut self,
        owner: &HomeKey,
        identity: &Pubkey,
        inner: Vec<AccountMeta>,
        data: Vec<u8>,
    ) -> Result<(), String> {
        let nonce = self.state(identity).nonce;
        let tx = self.act_tx(owner, identity, inner, data, nonce, self.env.now + 60);
        let r = self.relayer.insecure_clone();
        self.env.send(&tx, &[&r])
    }

    fn send_home_tx(
        &self,
        owner: &HomeKey,
        identity: &Pubkey,
        event: &Keypair,
        nonce: u64,
    ) -> Vec<Instruction> {
        let from = self.held(identity);
        let hash = solana_sha256_hasher::hashv(&[
            <safu_pool::instruction::CctpSendHome as anchor_lang::Discriminator>::DISCRIMINATOR,
            from.as_ref(),
        ])
        .to_bytes();
        let (signature, pre) = self.sign(owner, identity, nonce, self.env.now + 60, &hash);
        let home = owner.domain().to_string();
        let ix = self.env.ix(
            safu_pool::accounts::CctpSendHome {
                payer: self.relayer.pubkey(),
                pool: self.pool(),
                account: self.account(identity),
                identity: *identity,
                from,
                usdc_mint: CIRCLE_USDC,
                message_sent_event_data: event.pubkey(),
                instructions: IX_SYSVAR,
                token_program: TOKEN,
                system_program: SYSTEM,
                circle: safu_pool::accounts::CircleBurn {
                    message_transmitter_program: MT,
                    token_messenger_minter_program: TMM,
                    sender_authority: tmm_pda(&[b"sender_authority"]),
                    denylist_account: tmm_pda(&[b"denylist_account", identity.as_ref()]),
                    message_transmitter: mt_pda(&[b"message_transmitter"]),
                    token_messenger: tmm_pda(&[b"token_messenger"]),
                    remote_token_messenger: tmm_pda(&[b"remote_token_messenger", home.as_bytes()]),
                    token_minter: tmm_pda(&[b"token_minter"]),
                    local_token: tmm_pda(&[b"local_token", CIRCLE_USDC.as_ref()]),
                    token_messenger_event_authority: tmm_pda(&[b"__event_authority"]),
                },
            },
            safu_pool::instruction::CctpSendHome {
                nonce,
                expires_at: self.env.now + 60,
                signature,
            },
        );
        pre.into_iter().chain([ix]).collect()
    }

    /// Sends home and returns Circle's outbound message (from its message account).
    fn send_home(&mut self, owner: &HomeKey, identity: &Pubkey) -> Result<Vec<u8>, String> {
        let event = Keypair::new();
        let nonce = self.state(identity).nonce;
        let tx = self.send_home_tx(owner, identity, &event, nonce);
        let r = self.relayer.insecure_clone();
        self.env.send(&tx, &[&r, &event])?;
        let data = self.env.svm.get_account(&event.pubkey()).unwrap().data;
        // The message starts at its version and source domain (Solana).
        let mut head = c::MESSAGE_VERSION.to_be_bytes().to_vec();
        head.extend_from_slice(&c::domain::SOLANA.to_be_bytes());
        let at = data
            .windows(8)
            .position(|w| w == head.as_slice())
            .expect("message in event");
        Ok(data[at..].to_vec())
    }
}

fn read_32(m: &[u8], at: usize) -> [u8; 32] {
    m[at..at + 32].try_into().unwrap()
}

// --- receive --------------------------------------------------------------------------------------

#[test]
fn an_ethereum_burn_lands_in_the_owners_account_once() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let amount = 250 * USDC;
    let message = t.burn(&owner, c::MODE_STAKE, amount);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, message.clone());
    let r = t.relayer.insecure_clone();
    t.env.ok(std::slice::from_ref(&ix), &[&r]);
    let identity = t.identity(c::MODE_STAKE, &owner);
    let a = t.state(&identity);
    assert_eq!(
        (a.mode, a.home_domain, a.home_owner, a.to_deploy, a.nonce),
        (
            c::MODE_STAKE,
            c::domain::ETHEREUM,
            owner.bytes32(),
            amount,
            0
        )
    );
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
    assert_eq!(t.env.token_amount(&t.inbox(c::MODE_STAKE)), 0);
    // Circle marks the nonce used: the same message never mints twice.
    t.env.svm.expire_blockhash();
    assert!(t.env.send(&[ix], &[&r]).is_err());
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
}

#[test]
fn a_second_deposit_adds_to_the_same_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 10 * USDC);
    t.receive(c::MODE_BACK, &owner, 5 * USDC);
    assert_eq!(t.state(&identity).to_deploy, 15 * USDC);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 15 * USDC);
    // Each mode is its own account.
    assert_ne!(identity, t.identity(c::MODE_STAKE, &owner));
}

#[test]
fn receive_refuses_before_circle_so_the_message_stays_usable() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let r = t.relayer.insecure_clone();
    let (caller, stake_inbox) = (t.authority().to_bytes(), t.inbox(c::MODE_STAKE).to_bytes());

    // destinationCaller is someone else.
    let m = t.burn_to(&owner, c::domain::ETHEREUM, USDC, [9; 32], stake_inbox);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, m);
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongCaller);

    // Minted to the back inbox, received as a stake: the burn picked the mode.
    let m = t.burn(&owner, c::MODE_BACK, USDC);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, m.clone());
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongRecipient);
    // The same message, received as it was burned, still completes.
    let ix = t.receive_ix(c::MODE_BACK, &owner, m);
    t.env.ok(&[ix], &[&r]);

    // Claimed for another owner.
    let m = t.burn(&owner, c::MODE_STAKE, USDC);
    let ix = t.receive_ix(c::MODE_STAKE, &HomeKey::eth(), m);
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongOwner);

    // A Solana burner is not a home chain here.
    let m = t.burn_to(&owner, c::domain::SOLANA, USDC, caller, stake_inbox);
    let valid = t.burn(&owner, c::MODE_STAKE, USDC);
    let mut ix = t.receive_ix(c::MODE_STAKE, &owner, valid);
    let args = safu_pool::instruction::CctpReceive {
        mode: c::MODE_STAKE,
        home_domain: c::domain::ETHEREUM,
        home_owner: owner.bytes32(),
        message: m.clone(),
        attestation: t.attest(&m),
    };
    ix.data = args.data();
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpUnsupportedDomain);
}

// --- deploy ---------------------------------------------------------------------------------------

#[test]
fn deploy_stakes_as_the_owner_with_payouts_to_their_own_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let amount = bounds().1;
    let identity = t.receive(c::MODE_STAKE, &owner, amount);
    t.deploy(&identity, c::MODE_STAKE).unwrap();
    let s = t.env.stake_state(&identity);
    assert_eq!(
        (s.staker, s.beneficiary, s.amount),
        (identity, identity, amount)
    );
    assert_eq!(t.state(&identity).to_deploy, 0);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_err(
        t.deploy(&identity, c::MODE_STAKE),
        PoolError::CctpNothingToDeploy,
    );
}

#[test]
fn deploy_backs_in_back_mode() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 40 * USDC);
    t.deploy(&identity, c::MODE_BACK).unwrap();
    let b = t.env.backer_state(&identity);
    assert_eq!(b.backer, identity);
    assert_eq!(t.env.pool_state().total_backed + pending(&b), 40 * USDC);
}

fn pending(b: &safu_pool::state::BackerRecord) -> u64 {
    b.pending[..b.pending_count as usize]
        .iter()
        .map(|p| p.amount)
        .sum()
}

#[test]
fn deploy_must_use_the_owners_identity_and_held_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, bounds().1);
    let mut ix = t.deploy_ix(&identity, c::MODE_STAKE);
    // Someone else's USDC account in the staker's place.
    let at = t.call_accounts(&identity).len() + 5;
    ix.accounts[at].pubkey = t.env.ata(&t.relayer.pubkey());
    let r = t.relayer.insecure_clone();
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpCallNotAllowed);
}

#[test]
fn a_refused_deploy_leaves_the_usdc_held_and_it_can_go_home() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, bounds().1);
    let admin = t.env.admin.insecure_clone();
    let pause = t.env.admin_ix(safu_pool::instruction::Pause {});
    t.env.ok(&[pause], &[&admin]);
    assert_err(t.deploy(&identity, c::MODE_STAKE), PoolError::Paused);
    assert_eq!(t.state(&identity).to_deploy, bounds().1);
    assert_eq!(t.env.token_amount(&t.held(&identity)), bounds().1);
    t.send_home(&owner, &identity).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_eq!(t.state(&identity).to_deploy, 0);
}

// --- owner-signed calls ---------------------------------------------------------------------------

fn staked(t: &mut Cctp, owner: &HomeKey) -> Pubkey {
    let identity = t.receive(c::MODE_STAKE, owner, bounds().1);
    t.deploy(&identity, c::MODE_STAKE).unwrap();
    identity
}

#[test]
fn an_ethereum_owner_withdraws_into_their_own_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let amount = bounds().1;
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount }.data();
    t.act(&owner, &identity, inner, data).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
    assert_eq!(t.state(&identity).nonce, 1);
    assert_eq!(t.env.stake_state(&identity).amount, 0);
}

#[test]
fn a_stellar_owner_signs_with_sep53() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = staked(&mut t, &owner);
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
    t.act(&owner, &identity, inner, data).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), bounds().1);
}

#[test]
fn a_signature_works_once_for_its_owner_its_call_and_its_time() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let r = t.relayer.insecure_clone();
    let half = bounds().1 / 2;
    let data = safu_pool::instruction::Withdraw { amount: half }.data();
    let inner = t.withdraw_metas(&identity);
    let deadline = t.env.now + 60;

    // Someone else's key.
    let tx = t.act_tx(
        &HomeKey::eth(),
        &identity,
        inner.clone(),
        data.clone(),
        0,
        deadline,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadSignature);
    // A Stellar signature for an Ethereum owner.
    let tx = t.act_tx(
        &HomeKey::stellar(),
        &identity,
        inner.clone(),
        data.clone(),
        0,
        deadline,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpWrongSignatureKind);
    // Signed for one amount, sent with another.
    let mut tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    let other = safu_pool::instruction::Withdraw { amount: half - 1 }.data();
    let mut args = tx[0].data[8..].to_vec();
    let at = args.len() - data.len();
    args.splice(at.., other);
    tx[0].data.truncate(8);
    tx[0].data.extend(args);
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadSignature);
    // Too far out, then expired.
    let tx = t.act_tx(
        &owner,
        &identity,
        inner.clone(),
        data.clone(),
        0,
        t.env.now + MAX_ACTION_TTL_SECS + 1,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::SignatureDeadlineTooFar);
    let tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    t.env.warp(61);
    assert_err(t.env.send(&tx, &[&r]), PoolError::SignatureExpired);

    // A good one works once.
    let deadline = t.env.now + 60;
    let tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    t.env.ok(&tx, &[&r]);
    t.env.svm.expire_blockhash();
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadNonce);
}

#[test]
fn an_owner_cannot_send_payouts_anywhere_else() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let thief = t.env.funded(0).pubkey();

    // No beneficiary change at all.
    let set = safu_pool::accounts::SetBeneficiary {
        staker: identity,
        pool: t.pool(),
        stake_record: t.env.stake_record(&identity),
    };
    let inner = Cctp::as_identity(set.to_account_metas(None), &identity);
    let data = safu_pool::instruction::SetBeneficiary { beneficiary: thief }.data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::CctpCallNotAllowed,
    );

    // A withdrawal into someone else's USDC account: the pool pays only the beneficiary.
    t.env.open_usdc(&thief);
    let mut inner = t.withdraw_metas(&identity);
    inner[4].pubkey = t.env.ata(&thief);
    let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::WrongBeneficiary,
    );

    // A new stake naming someone else as beneficiary.
    let held = t.held(&identity);
    t.env.add_token(&held, USDC);
    let inner = t.stake_metas(&identity);
    let data = safu_pool::instruction::Stake {
        amount: USDC,
        beneficiary: thief,
    }
    .data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::CctpBeneficiaryNotOwn,
    );
}

// --- send home ------------------------------------------------------------------------------------

#[test]
fn ethereum_owners_get_their_usdc_back_on_ethereum() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, 12 * USDC);
    let m = t.send_home(&owner, &identity).unwrap();
    let body = c::header::BODY;
    assert_eq!(&m[8..12], &c::domain::ETHEREUM.to_be_bytes());
    assert_eq!(read_32(&m, c::header::DESTINATION_CALLER), [0u8; 32]);
    assert_eq!(read_32(&m, body + c::burn::MINT_RECIPIENT), owner.bytes32());
    assert_eq!(
        read_32(&m, body + c::burn::MESSAGE_SENDER),
        identity.to_bytes()
    );
    assert_eq!(
        &m[body + c::burn::AMOUNT + 24..body + c::burn::AMOUNT + 32],
        &(12 * USDC).to_be_bytes()
    );
    assert_eq!(m.len(), body + c::burn::HOOK_DATA);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_err(
        t.send_home(&owner, &identity).map(|_| ()),
        PoolError::CctpNothingToSend,
    );
}

#[test]
fn stellar_owners_get_theirs_through_circles_forwarder() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 7 * USDC);
    let m = t.send_home(&owner, &identity).unwrap();
    let forwarder = t.env.pool_state().stellar_forwarder;
    let body = c::header::BODY;
    assert_eq!(&m[8..12], &c::domain::STELLAR.to_be_bytes());
    assert_eq!(read_32(&m, c::header::DESTINATION_CALLER), forwarder);
    assert_eq!(read_32(&m, body + c::burn::MINT_RECIPIENT), forwarder);
    let owner_key: [u8; 32] = owner.bytes32();
    assert_eq!(
        &m[body + c::burn::HOOK_DATA..],
        &c::forward_hook(&owner_key)
    );
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
}

#[test]
fn only_the_owner_can_send_home() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, 3 * USDC);
    let event = Keypair::new();
    let tx = t.send_home_tx(&HomeKey::eth(), &identity, &event, 0);
    let r = t.relayer.insecure_clone();
    assert_err(t.env.send(&tx, &[&r, &event]), PoolError::CctpBadSignature);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 3 * USDC);
}

#[test]
fn the_pool_keeps_its_stellar_forwarder_from_config() {
    let t = Cctp::new();
    let p: Pool = t.env.pool_state();
    assert_eq!(p.stellar_forwarder, config().stellar_forwarder);
    assert_ne!(p.stellar_forwarder, [0u8; 32]);
}

/// The owner-signed path costs little on top of the call it runs: the busiest user call measures
/// 260-263k (`kamino.rs`, harvest + push + upkeep), so with this extra it still fits the site's
/// 400k limit. Receive (about 240k, mostly Circle checking two attester signatures and minting),
/// deploy and send home fit on their own.
#[test]
fn owner_calls_and_bridging_fit_the_site_limit() {
    let mut t = Cctp::new();
    let limit = t.env.cfg.compute_unit_limit;
    let r = t.relayer.insecure_clone();
    for owner in [HomeKey::eth(), HomeKey::stellar()] {
        let m = t.burn(&owner, c::MODE_STAKE, bounds().1);
        let ix = t.receive_ix(c::MODE_STAKE, &owner, m);
        let receive = t.env.send_cu(&[ix], &[&r]).unwrap();
        let identity = t.identity(c::MODE_STAKE, &owner);
        let ix = t.deploy_ix(&identity, c::MODE_STAKE);
        let deploy = t.env.send_cu(&[ix], &[&r]).unwrap();

        let nonce = t.state(&identity).nonce;
        let inner = t.withdraw_metas(&identity);
        let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
        let tx = t.act_tx(&owner, &identity, inner, data, nonce, t.env.now + 60);
        let act = t.env.send_cu(&tx, &[&r]).unwrap();
        let plain = t.env.staker(bounds().1);
        let ix = t.env.withdraw_ix(&plain.pubkey(), &plain.pubkey());
        let base = t.env.send_cu(&[ix], &[&plain]).unwrap();

        let event = Keypair::new();
        let tx = t.send_home_tx(&owner, &identity, &event, nonce + 1);
        let home = t.env.send_cu(&tx, &[&r, &event]).unwrap();
        println!(
            "domain {}: receive {receive}, deploy {deploy}, act {act} (+{}), send home {home}",
            owner.domain(),
            act - base
        );
        assert!(act - base <= 60_000, "owner-signed overhead {}", act - base);
        for cu in [receive, deploy, act, home] {
            assert!(cu <= limit, "{cu} of {limit}");
        }
    }
}
