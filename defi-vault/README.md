# DeFi Token Vault — Solana Anchor

A production-grade staking vault built with the Anchor framework on Solana. Users deposit SPL tokens, stake them, and earn streaming reward tokens using the battle-tested **reward-per-token accumulator** pattern (Synthetix / MasterChef design).

---

## Features

- **Deposit & Withdraw** — SPL token vault secured by PDAs
- **Streaming Rewards** — Reward tokens accrue per second proportional to stake share
- **Claim Rewards** — Claim without touching principal
- **Admin Controls** — Update reward rate, emergency pause
- **On-chain Events** — All state changes emit events for indexers and front-ends
- **Checked Arithmetic** — No silent overflows; all math uses `checked_*` ops
- **Full Test Suite** — TypeScript integration tests with Anchor + `@solana/spl-token`

---

## Architecture

```
defi-vault/
├── Anchor.toml
├── programs/
│   └── defi-vault/
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs          # Full program logic
└── tests/
    └── defi-vault.ts           # Integration tests
```

### Key Accounts

| Account | Type | Description |
|---|---|---|
| `VaultState` | PDA | Global config: admin, mints, reward rate, accumulator |
| `UserPosition` | PDA | Per-user stake, reward debt, pending rewards |
| `vault_treasury` | Token PDA | Holds deposited tokens (authority = VaultState) |
| `reward_treasury` | Token PDA | Holds reward tokens (authority = VaultState) |

### PDA Seeds

```
vault_state     → ["vault_state",    deposit_mint]
vault_treasury  → ["vault_treasury", deposit_mint]
reward_treasury → ["reward_treasury", reward_mint]
user_position   → ["user_position",  vault_state, user_pubkey]
```

---

## Reward Math

This vault uses the standard **reward-per-token accumulator** pattern to distribute rewards fairly and gas-efficiently without iterating over users.

```
reward_per_token_delta = elapsed_seconds × rate × PRECISION / total_staked

earned = staked_amount × (reward_per_token_stored − user_reward_debt) / PRECISION
```

`PRECISION = 1_000_000_000` (1e9) is used as a fixed-point scalar to prevent integer truncation.

The accumulator is updated globally on every deposit, withdrawal, or claim — ensuring rewards are always settled before balances change.

---

## Instructions

| Instruction | Who | Description |
|---|---|---|
| `initialize` | Admin | Deploy vault, set reward rate |
| `deposit` | User | Stake tokens, auto-settle rewards |
| `withdraw` | User | Unstake tokens + auto-settle rewards |
| `claim_rewards` | User | Claim accrued reward tokens |
| `set_reward_rate` | Admin | Adjust emission rate (settles accumulator first) |
| `set_paused` | Admin | Emergency pause / unpause |

---

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) `>=1.75`
- [Solana CLI](https://docs.solanalabs.com/cli/install) `>=1.18`
- [Anchor CLI](https://www.anchor-lang.com/docs/installation) `>=0.29`
- Node.js `>=18` + Yarn

---

## Getting Started

```bash
# 1. Clone
git clone https://github.com/YOUR_USERNAME/defi-vault
cd defi-vault

# 2. Install JS dependencies
yarn install

# 3. Build the program
anchor build

# 4. Run a local validator and tests
anchor test
```

### Deploy to Devnet

```bash
# Switch cluster
solana config set --url devnet

# Airdrop SOL for fees
solana airdrop 2

# Deploy
anchor deploy --provider.cluster devnet
```

---

## Security Considerations

- **Checked arithmetic** — all math uses `checked_add` / `checked_sub` / `checked_mul` / `checked_div` with `VaultError::MathOverflow` on failure
- **PDA authority** — the vault treasury token accounts are owned by the `VaultState` PDA; no external key can sign withdrawals
- **Constraint guards** — unauthorized calls revert at account validation before instruction logic runs
- **Accumulator settlement** — global accumulator is always updated *before* any balance mutation to prevent reward manipulation
- **Pause mechanism** — admin can halt deposits in emergencies without affecting withdrawals or claims

> ⚠️ This contract is for demonstration and educational purposes. It has not been audited. Do not deploy to mainnet with real funds without a professional security audit.

---

## Events

All instructions emit on-chain events consumable by off-chain indexers (e.g. The Graph, Helius webhooks):

```
VaultInitialized  { admin, deposit_mint, reward_rate_per_second }
Deposited         { user, amount, total_user_staked }
Withdrawn         { user, amount, remaining_staked }
RewardsClaimed    { user, amount }
RewardRateUpdated { old_rate, new_rate }
VaultPauseToggled { paused }
```

---

## Tech Stack

| Layer | Technology |
|---|---|
| Smart Contract | Rust, Anchor 0.29 |
| Token Standard | SPL Token (Solana) |
| Tests | TypeScript, Mocha, Chai |
| Local Validator | Solana Test Validator (via `anchor test`) |

---

## Author

**PJ** — Smart Contract Engineer  
EVM (Solidity, Hardhat, Foundry) · Solana (Anchor, Rust) · DeFi Protocol Design  
