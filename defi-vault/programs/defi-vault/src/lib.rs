use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("Fg6PaFpoGXkYsidMpWTK6W2BeZ7FEfcYkg476zPFsLnS");

// ============================================================
//  DeFi Vault — Deposit, Stake, and Earn Rewards
//  Demonstrates: PDAs, CPIs, token accounts, math safety,
//  access control, and event emission — all interview staples.
// ============================================================

#[program]
pub mod defi_vault {
    use super::*;

    /// Initialize the global vault state and treasury accounts.
    /// Called once by the protocol admin.
    pub fn initialize(
        ctx: Context<Initialize>,
        reward_rate_per_second: u64, // e.g. 1_000 = 0.001 tokens/sec per token staked
    ) -> Result<()> {
        let vault_state = &mut ctx.accounts.vault_state;
        vault_state.admin = ctx.accounts.admin.key();
        vault_state.deposit_mint = ctx.accounts.deposit_mint.key();
        vault_state.reward_mint = ctx.accounts.reward_mint.key();
        vault_state.reward_rate_per_second = reward_rate_per_second;
        vault_state.total_staked = 0;
        vault_state.last_update_time = Clock::get()?.unix_timestamp as u64;
        vault_state.reward_per_token_stored = 0;
        vault_state.bump = ctx.bumps.vault_state;

        emit!(VaultInitialized {
            admin: vault_state.admin,
            deposit_mint: vault_state.deposit_mint,
            reward_rate_per_second,
        });

        Ok(())
    }

    /// Deposit tokens into the vault and create/update a user position.
    /// User receives a "share" tracked in UserPosition.
    pub fn deposit(ctx: Context<Deposit>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::ZeroAmount);

        let vault_state = &mut ctx.accounts.vault_state;
        let user_position = &mut ctx.accounts.user_position;
        let now = Clock::get()?.unix_timestamp as u64;

        // 1. Update global reward accumulator before mutating balances
        update_reward_per_token(vault_state, now)?;

        // 2. Settle pending rewards for this user before changing their stake
        settle_pending_rewards(vault_state, user_position)?;

        // 3. Transfer deposit tokens: user → vault treasury (CPI)
        let cpi_ctx = CpiContext::new(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.user_token_account.to_account_info(),
                to: ctx.accounts.vault_treasury.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            },
        );
        token::transfer(cpi_ctx, amount)?;

        // 4. Update state
        vault_state.total_staked = vault_state
            .total_staked
            .checked_add(amount)
            .ok_or(VaultError::MathOverflow)?;

        user_position.owner = ctx.accounts.user.key();
        user_position.staked_amount = user_position
            .staked_amount
            .checked_add(amount)
            .ok_or(VaultError::MathOverflow)?;
        user_position.reward_debt = vault_state.reward_per_token_stored;
        user_position.last_update = now;

        emit!(Deposited {
            user: ctx.accounts.user.key(),
            amount,
            total_user_staked: user_position.staked_amount,
        });

        Ok(())
    }

    /// Withdraw staked tokens. Automatically claims pending rewards.
    pub fn withdraw(ctx: Context<Withdraw>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::ZeroAmount);

        let user_position = &ctx.accounts.user_position;
        require!(
            user_position.staked_amount >= amount,
            VaultError::InsufficientBalance
        );

        let vault_state = &mut ctx.accounts.vault_state;
        let user_position = &mut ctx.accounts.user_position;
        let now = Clock::get()?.unix_timestamp as u64;

        // Update rewards before changing stake
        update_reward_per_token(vault_state, now)?;
        settle_pending_rewards(vault_state, user_position)?;

        // CPI: vault treasury → user (PDA signs)
        let seeds = &[
            b"vault_state".as_ref(),
            vault_state.deposit_mint.as_ref(),
            &[vault_state.bump],
        ];
        let signer_seeds = &[&seeds[..]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault_treasury.to_account_info(),
                to: ctx.accounts.user_token_account.to_account_info(),
                authority: ctx.accounts.vault_state.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi_ctx, amount)?;

        vault_state.total_staked = vault_state
            .total_staked
            .checked_sub(amount)
            .ok_or(VaultError::MathOverflow)?;
        user_position.staked_amount = user_position
            .staked_amount
            .checked_sub(amount)
            .ok_or(VaultError::MathOverflow)?;
        user_position.reward_debt = vault_state.reward_per_token_stored;

        emit!(Withdrawn {
            user: ctx.accounts.user.key(),
            amount,
            remaining_staked: user_position.staked_amount,
        });

        Ok(())
    }

    /// Claim accrued reward tokens without touching the staked principal.
    pub fn claim_rewards(ctx: Context<ClaimRewards>) -> Result<()> {
        let vault_state = &mut ctx.accounts.vault_state;
        let user_position = &mut ctx.accounts.user_position;
        let now = Clock::get()?.unix_timestamp as u64;

        update_reward_per_token(vault_state, now)?;

        let earned = calculate_earned(vault_state, user_position)?;
        require!(earned > 0, VaultError::NoRewardsToClaim);

        // CPI: reward treasury → user (PDA signs)
        let seeds = &[
            b"vault_state".as_ref(),
            vault_state.deposit_mint.as_ref(),
            &[vault_state.bump],
        ];
        let signer_seeds = &[&seeds[..]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.reward_treasury.to_account_info(),
                to: ctx.accounts.user_reward_account.to_account_info(),
                authority: ctx.accounts.vault_state.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi_ctx, earned)?;

        user_position.rewards_claimed = user_position
            .rewards_claimed
            .checked_add(earned)
            .ok_or(VaultError::MathOverflow)?;
        user_position.reward_debt = vault_state.reward_per_token_stored;
        user_position.pending_rewards = 0;

        emit!(RewardsClaimed {
            user: ctx.accounts.user.key(),
            amount: earned,
        });

        Ok(())
    }

    /// Admin: update reward emission rate (governance action).
    pub fn set_reward_rate(ctx: Context<AdminAction>, new_rate: u64) -> Result<()> {
        let vault_state = &mut ctx.accounts.vault_state;
        let now = Clock::get()?.unix_timestamp as u64;

        // Settle existing accumulator before changing rate
        update_reward_per_token(vault_state, now)?;

        let old_rate = vault_state.reward_rate_per_second;
        vault_state.reward_rate_per_second = new_rate;

        emit!(RewardRateUpdated { old_rate, new_rate });
        Ok(())
    }

    /// Admin: pause the vault in emergencies.
    pub fn set_paused(ctx: Context<AdminAction>, paused: bool) -> Result<()> {
        ctx.accounts.vault_state.paused = paused;
        emit!(VaultPauseToggled { paused });
        Ok(())
    }
}

// ============================================================
//  Internal math helpers
// ============================================================

/// Global accumulator: tracks reward tokens earned per staked token since genesis.
/// Uses fixed-point arithmetic scaled by PRECISION to avoid integer truncation.
fn update_reward_per_token(vault_state: &mut VaultState, now: u64) -> Result<()> {
    if vault_state.total_staked == 0 {
        vault_state.last_update_time = now;
        return Ok(());
    }

    let elapsed = now.saturating_sub(vault_state.last_update_time);
    if elapsed == 0 {
        return Ok(());
    }

    // reward_per_token_delta = elapsed * rate * PRECISION / total_staked
    let reward_delta = (elapsed as u128)
        .checked_mul(vault_state.reward_rate_per_second as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_mul(PRECISION as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(vault_state.total_staked as u128)
        .ok_or(VaultError::MathOverflow)? as u64;

    vault_state.reward_per_token_stored = vault_state
        .reward_per_token_stored
        .checked_add(reward_delta)
        .ok_or(VaultError::MathOverflow)?;
    vault_state.last_update_time = now;

    Ok(())
}

/// Moves any newly accrued rewards into user_position.pending_rewards.
fn settle_pending_rewards(vault_state: &VaultState, user_position: &mut UserPosition) -> Result<()> {
    let newly_earned = calculate_earned(vault_state, user_position)?;
    user_position.pending_rewards = user_position
        .pending_rewards
        .checked_add(newly_earned)
        .ok_or(VaultError::MathOverflow)?;
    Ok(())
}

/// earned = staked * (global_rpt - user_debt) / PRECISION
fn calculate_earned(vault_state: &VaultState, user_position: &UserPosition) -> Result<u64> {
    let delta = vault_state
        .reward_per_token_stored
        .saturating_sub(user_position.reward_debt);

    let earned = (user_position.staked_amount as u128)
        .checked_mul(delta as u128)
        .ok_or(VaultError::MathOverflow)?
        .checked_div(PRECISION as u128)
        .ok_or(VaultError::MathOverflow)? as u64;

    Ok(earned.checked_add(user_position.pending_rewards).ok_or(VaultError::MathOverflow)?)
}

const PRECISION: u64 = 1_000_000_000; // 1e9 fixed-point scale

// ============================================================
//  Account structs
// ============================================================

#[account]
#[derive(Default)]
pub struct VaultState {
    pub admin: Pubkey,                   // 32
    pub deposit_mint: Pubkey,            // 32
    pub reward_mint: Pubkey,             // 32
    pub reward_rate_per_second: u64,     // 8
    pub total_staked: u64,               // 8
    pub last_update_time: u64,           // 8
    pub reward_per_token_stored: u64,    // 8
    pub paused: bool,                    // 1
    pub bump: u8,                        // 1
}

#[account]
#[derive(Default)]
pub struct UserPosition {
    pub owner: Pubkey,          // 32
    pub staked_amount: u64,     // 8
    pub reward_debt: u64,       // 8  — snapshot of reward_per_token_stored at last update
    pub pending_rewards: u64,   // 8  — settled but unclaimed rewards
    pub rewards_claimed: u64,   // 8  — lifetime claimed (useful for front-ends)
    pub last_update: u64,       // 8
}

// ============================================================
//  Instruction contexts
// ============================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + std::mem::size_of::<VaultState>(),
        seeds = [b"vault_state", deposit_mint.key().as_ref()],
        bump
    )]
    pub vault_state: Account<'info, VaultState>,

    /// Vault-owned treasury for deposit tokens (PDA token account)
    #[account(
        init,
        payer = admin,
        token::mint = deposit_mint,
        token::authority = vault_state,
        seeds = [b"vault_treasury", deposit_mint.key().as_ref()],
        bump
    )]
    pub vault_treasury: Account<'info, TokenAccount>,

    /// Vault-owned treasury for reward tokens
    #[account(
        init,
        payer = admin,
        token::mint = reward_mint,
        token::authority = vault_state,
        seeds = [b"reward_treasury", reward_mint.key().as_ref()],
        bump
    )]
    pub reward_treasury: Account<'info, TokenAccount>,

    pub deposit_mint: Account<'info, Mint>,
    pub reward_mint: Account<'info, Mint>,

    #[account(mut)]
    pub admin: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    #[account(
        mut,
        seeds = [b"vault_state", vault_state.deposit_mint.as_ref()],
        bump = vault_state.bump,
        constraint = !vault_state.paused @ VaultError::VaultPaused,
    )]
    pub vault_state: Account<'info, VaultState>,

    #[account(
        init_if_needed,
        payer = user,
        space = 8 + std::mem::size_of::<UserPosition>(),
        seeds = [b"user_position", vault_state.key().as_ref(), user.key().as_ref()],
        bump
    )]
    pub user_position: Account<'info, UserPosition>,

    #[account(
        mut,
        seeds = [b"vault_treasury", vault_state.deposit_mint.as_ref()],
        bump,
        token::mint = vault_state.deposit_mint,
    )]
    pub vault_treasury: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = vault_state.deposit_mint,
        token::authority = user,
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(
        mut,
        seeds = [b"vault_state", vault_state.deposit_mint.as_ref()],
        bump = vault_state.bump,
    )]
    pub vault_state: Account<'info, VaultState>,

    #[account(
        mut,
        seeds = [b"user_position", vault_state.key().as_ref(), user.key().as_ref()],
        bump,
        constraint = user_position.owner == user.key() @ VaultError::Unauthorized,
    )]
    pub user_position: Account<'info, UserPosition>,

    #[account(
        mut,
        seeds = [b"vault_treasury", vault_state.deposit_mint.as_ref()],
        bump,
    )]
    pub vault_treasury: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = vault_state.deposit_mint,
        token::authority = user,
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClaimRewards<'info> {
    #[account(
        mut,
        seeds = [b"vault_state", vault_state.deposit_mint.as_ref()],
        bump = vault_state.bump,
    )]
    pub vault_state: Account<'info, VaultState>,

    #[account(
        mut,
        seeds = [b"user_position", vault_state.key().as_ref(), user.key().as_ref()],
        bump,
        constraint = user_position.owner == user.key() @ VaultError::Unauthorized,
    )]
    pub user_position: Account<'info, UserPosition>,

    #[account(
        mut,
        seeds = [b"reward_treasury", vault_state.reward_mint.as_ref()],
        bump,
    )]
    pub reward_treasury: Account<'info, TokenAccount>,

    #[account(
        mut,
        token::mint = vault_state.reward_mint,
        token::authority = user,
    )]
    pub user_reward_account: Account<'info, TokenAccount>,

    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct AdminAction<'info> {
    #[account(
        mut,
        seeds = [b"vault_state", vault_state.deposit_mint.as_ref()],
        bump = vault_state.bump,
        constraint = vault_state.admin == admin.key() @ VaultError::Unauthorized,
    )]
    pub vault_state: Account<'info, VaultState>,

    pub admin: Signer<'info>,
}

// ============================================================
//  Events  (indexed by off-chain indexers / UI listeners)
// ============================================================

#[event]
pub struct VaultInitialized {
    pub admin: Pubkey,
    pub deposit_mint: Pubkey,
    pub reward_rate_per_second: u64,
}

#[event]
pub struct Deposited {
    pub user: Pubkey,
    pub amount: u64,
    pub total_user_staked: u64,
}

#[event]
pub struct Withdrawn {
    pub user: Pubkey,
    pub amount: u64,
    pub remaining_staked: u64,
}

#[event]
pub struct RewardsClaimed {
    pub user: Pubkey,
    pub amount: u64,
}

#[event]
pub struct RewardRateUpdated {
    pub old_rate: u64,
    pub new_rate: u64,
}

#[event]
pub struct VaultPauseToggled {
    pub paused: bool,
}

// ============================================================
//  Custom errors
// ============================================================

#[error_code]
pub enum VaultError {
    #[msg("Amount must be greater than zero")]
    ZeroAmount,
    #[msg("Insufficient staked balance")]
    InsufficientBalance,
    #[msg("No rewards available to claim")]
    NoRewardsToClaim,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Caller is not authorized")]
    Unauthorized,
    #[msg("Vault is currently paused")]
    VaultPaused,
}
