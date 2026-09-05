use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;

declare_id!("KrpTk4R9wZJ1B5S8H1G2R4P8W3Y6V9L2N7C5M1Q3E8A");

pub const MAX_ORACLE_STALENESS_SECONDS: i64 = 30;
pub const MAX_CONF_INTERVAL_BPS: u64 = 50; // 0.50% max confidence width
pub const BPS_DIVISOR: u64 = 10_000;

#[program]
pub mod kriptok_league {
    use super::*;

    /// Initializes the global protocol configuration.
    pub fn initialize_protocol(
        ctx: Context<InitializeProtocol>,
        fee_basis_points: u16,
        min_stake: u64,
        max_stake: u64,
    ) -> Result<()> {
        require!(fee_basis_points <= 1000, KriptoKError::ExcessiveFeeBps);
        require!(min_stake > 0 && max_stake >= min_stake, KriptoKError::InvalidStakeLimits);

        let config = &mut ctx.accounts.config;
        config.admin = ctx.accounts.admin.key();
        config.treasury = ctx.accounts.treasury.key();
        config.mint = ctx.accounts.mint.key();
        config.fee_basis_points = fee_basis_points;
        config.min_stake = min_stake;
        config.max_stake = max_stake;
        config.bump = ctx.bumps.config;

        emit!(ProtocolInitializedEvent {
            admin: config.admin,
            treasury: config.treasury,
            fee_basis_points,
        });
        Ok(())
    }

    /// Initializes a new timed trading round (e.g., 60 seconds, 5 minutes).
    pub fn initialize_round(
        ctx: Context<InitializeRound>,
        round_id: u64,
        start_time: i64,
        lock_time: i64,
        settle_time: i64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        require!(start_time >= clock.unix_timestamp, KriptoKError::InvalidTimestamps);
        require!(lock_time > start_time, KriptoKError::InvalidTimestamps);
        require!(settle_time > lock_time, KriptoKError::InvalidTimestamps);

        let round = &mut ctx.accounts.round;
        round.round_id = round_id;
        round.creator = ctx.accounts.creator.key();
        round.start_time = start_time;
        round.lock_time = lock_time;
        round.settle_time = settle_time;
        round.strike_price = 0;
        round.resolution_price = 0;
        round.total_bull_amount = 0;
        round.total_bear_amount = 0;
        round.status = RoundStatus::Open;
        round.bump = ctx.bumps.round;
        round.vault_bump = ctx.bumps.round_vault;

        emit!(RoundCreatedEvent {
            round_id,
            start_time,
            lock_time,
            settle_time,
        });
        Ok(())
    }

    /// Trader joins a round with a directional stake (Bull or Bear).
    pub fn enter_position(
        ctx: Context<EnterPosition>,
        round_id: u64,
        direction: Direction,
        amount: u64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;
        let config = &ctx.accounts.config;

        require!(round.status == RoundStatus::Open, KriptoKError::RoundNotOpen);
        require!(clock.unix_timestamp < round.lock_time, KriptoKError::RoundLocked);
        require!(amount >= config.min_stake, KriptoKError::StakeTooSmall);
        require!(amount <= config.max_stake, KriptoKError::StakeTooLarge);

        // CPI: Transfer tokens from trader to round vault PDA
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.round_vault.to_account_info(),
            authority: ctx.accounts.trader.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Update pool counters
        match direction {
            Direction::Bull => {
                round.total_bull_amount = round
                    .total_bull_amount
                    .checked_add(amount)
                    .ok_or(KriptoKError::MathOverflow)?;
            }
            Direction::Bear => {
                round.total_bear_amount = round
                    .total_bear_amount
                    .checked_add(amount)
                    .ok_or(KriptoKError::MathOverflow)?;
            }
        }

        // Initialize User Position Record
        let position = &mut ctx.accounts.position;
        position.round_id = round_id;
        position.trader = ctx.accounts.trader.key();
        position.direction = direction;
        position.amount = amount;
        position.claimed = false;
        position.bump = ctx.bumps.position;

        emit!(PositionEnteredEvent {
            round_id,
            trader: position.trader,
            direction,
            amount,
        });

        Ok(())
    }

    /// Locks the round at lock_time and sets strike price from Pyth oracle feed.
    pub fn lock_round(ctx: Context<LockRound>, _round_id: u64) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;

        require!(round.status == RoundStatus::Open, KriptoKError::RoundNotOpen);
        require!(clock.unix_timestamp >= round.lock_time, KriptoKError::LockTimeNotReached);
        require!(clock.unix_timestamp < round.settle_time, KriptoKError::SettlementTimePassed);

        // Fetch validated Pyth price
        let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
            .map_err(|_| KriptoKError::InvalidOracleAccount)?;
        let current_price = price_feed
            .get_price_no_older_than(clock.unix_timestamp, MAX_ORACLE_STALENESS_SECONDS)
            .ok_or(KriptoKError::StaleOraclePrice)?;

        validate_pyth_confidence(&current_price)?;

        round.strike_price = current_price.price;
        round.status = RoundStatus::Locked;

        emit!(RoundLockedEvent {
            round_id: round.round_id,
            strike_price: current_price.price,
            timestamp: clock.unix_timestamp,
        });
        Ok(())
    }

    /// Resolves the round after settle_time and computes win/loss status.
    pub fn settle_round(ctx: Context<SettleRound>, round_id: u64) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;

        require!(round.status == RoundStatus::Locked, KriptoKError::RoundNotLocked);
        require!(clock.unix_timestamp >= round.settle_time, KriptoKError::SettlementTimeNotReached);

        // Fetch final resolution price from Pyth oracle
        let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
            .map_err(|_| KriptoKError::InvalidOracleAccount)?;
        let current_price = price_feed
            .get_price_no_older_than(clock.unix_timestamp, MAX_ORACLE_STALENESS_SECONDS)
            .ok_or(KriptoKError::StaleOraclePrice)?;

        validate_pyth_confidence(&current_price)?;

        round.resolution_price = current_price.price;

        if round.resolution_price > round.strike_price {
            round.status = RoundStatus::SettledBullWon;
        } else if round.resolution_price < round.strike_price {
            round.status = RoundStatus::SettledBearWon;
        } else {
            round.status = RoundStatus::SettledDraw;
        }

        emit!(RoundSettledEvent {
            round_id,
            strike_price: round.strike_price,
            resolution_price: round.resolution_price,
            status: round.status,
        });
        Ok(())
    }

    /// Winner claims principal and proportional share of losing pool, minus fee.
    pub fn claim_winnings(ctx: Context<ClaimWinnings>, round_id: u64) -> Result<()> {
        let round = &ctx.accounts.round;
        let position = &mut ctx.accounts.position;
        let config = &ctx.accounts.config;

        require!(!position.claimed, KriptoKError::AlreadyClaimed);
        require!(
            round.status == RoundStatus::SettledBullWon
                || round.status == RoundStatus::SettledBearWon
                || round.status == RoundStatus::SettledDraw,
            KriptoKError::RoundNotSettled
        );

        let mut payout_amount: u64 = 0;
        let mut fee_deduction: u64 = 0;

        match round.status {
            RoundStatus::SettledDraw => {
                // Refund exact stake on draw
                payout_amount = position.amount;
            }
            RoundStatus::SettledBullWon => {
                if position.direction == Direction::Bull {
                    payout_amount = calculate_payout(
                        position.amount,
                        round.total_bull_amount,
                        round.total_bear_amount,
                        config.fee_basis_points,
                        &mut fee_deduction,
                    )?;
                }
            }
            RoundStatus::SettledBearWon => {
                if position.direction == Direction::Bear {
                    payout_amount = calculate_payout(
                        position.amount,
                        round.total_bear_amount,
                        round.total_bull_amount,
                        config.fee_basis_points,
                        &mut fee_deduction,
                    )?;
                }
            }
            _ => return Err(KriptoKError::RoundNotSettled.into()),
        }

        require!(payout_amount > 0, KriptoKError::NoWinningsToClaim);
        position.claimed = true;

        let round_id_bytes = round_id.to_le_bytes();
        let seeds = &[
            b"vault".as_ref(),
            round_id_bytes.as_ref(),
            &[round.vault_bump],
        ];
        let signer_seeds = &[&seeds[..]];

        // Transfer Fee to Treasury
        if fee_deduction > 0 {
            let fee_cpi = Transfer {
                from: ctx.accounts.round_vault.to_account_info(),
                to: ctx.accounts.treasury_token_account.to_account_info(),
                authority: ctx.accounts.round_vault.to_account_info(),
            };
            let fee_ctx = CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                fee_cpi,
                signer_seeds,
            );
            token::transfer(fee_ctx, fee_deduction)?;
        }

        // Transfer Payout to Winner
        let payout_cpi = Transfer {
            from: ctx.accounts.round_vault.to_account_info(),
            to: ctx.accounts.user_token_account.to_account_info(),
            authority: ctx.accounts.round_vault.to_account_info(),
        };
        let payout_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            payout_cpi,
            signer_seeds,
        );
        token::transfer(payout_ctx, payout_amount)?;

        emit!(WinningsClaimedEvent {
            round_id,
            trader: position.trader,
            payout_amount,
            fee_deduction,
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Pure Calculation Helpers
// -----------------------------------------------------------------------------
fn calculate_payout(
    user_stake: u64,
    winning_pool_total: u64,
    losing_pool_total: u64,
    fee_bps: u16,
    fee_deduction: &mut u64,
) -> Result<u64> {
    if winning_pool_total == 0 {
        return Err(KriptoKError::MathOverflow.into());
    }

    // Gross prize share = losing_pool_total * (user_stake / winning_pool_total)
    let gross_profit = (losing_pool_total as u128)
        .checked_mul(user_stake as u128)
        .ok_or(KriptoKError::MathOverflow)?
        .checked_div(winning_pool_total as u128)
        .ok_or(KriptoKError::MathOverflow)? as u64;

    let fee = (gross_profit as u128)
        .checked_mul(fee_bps as u128)
        .ok_or(KriptoKError::MathOverflow)?
        .checked_div(BPS_DIVISOR as u128)
        .ok_or(KriptoKError::MathOverflow)? as u64;

    *fee_deduction = fee;
    let net_profit = gross_profit.checked_sub(fee).ok_or(KriptoKError::MathOverflow)?;
    let total_payout = user_stake.checked_add(net_profit).ok_or(KriptoKError::MathOverflow)?;

    Ok(total_payout)
}

fn validate_pyth_confidence(price: &pyth_sdk_solana::Price) -> Result<()> {
    if price.price <= 0 {
        return Err(KriptoKError::InvalidOraclePrice.into());
    }
    // conf / price <= MAX_CONF_INTERVAL_BPS / BPS_DIVISOR
    let conf_bps = (price.conf as u128)
        .checked_mul(BPS_DIVISOR as u128)
        .ok_or(KriptoKError::MathOverflow)?
        .checked_div(price.price as u128)
        .ok_or(KriptoKError::MathOverflow)? as u64;

    require!(conf_bps <= MAX_CONF_INTERVAL_BPS, KriptoKError::PythConfidenceTooWide);
    Ok(())
}

// -----------------------------------------------------------------------------
// Account Context Structs
// -----------------------------------------------------------------------------
#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + ProtocolConfig::LEN,
        seeds = [b"config"],
        bump
    )]
    pub config: Account<'info, ProtocolConfig>,
    pub mint: Account<'info, Mint>,
    /// CHECK: Protocol Treasury Account
    pub treasury: AccountInfo<'info>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct InitializeRound<'info> {
    #[account(
        init,
        payer = creator,
        space = 8 + RoundAccount::LEN,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump
    )]
    pub round: Account<'info, RoundAccount>,
    #[account(
        init,
        payer = creator,
        seeds = [b"vault", round_id.to_le_bytes().as_ref()],
        bump,
        token::mint = mint,
        token::authority = round_vault
    )]
    pub round_vault: Account<'info, TokenAccount>,
    pub mint: Account<'info, Mint>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct EnterPosition<'info> {
    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, RoundAccount>,
    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, ProtocolConfig>,
    #[account(
        init,
        payer = trader,
        space = 8 + PositionAccount::LEN,
        seeds = [b"position", round_id.to_le_bytes().as_ref(), trader.key().as_ref()],
        bump
    )]
    pub position: Account<'info, PositionAccount>,
    #[account(
        mut,
        seeds = [b"vault", round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = user_token_account.owner == trader.key() @ KriptoKError::UnauthorizedAccount,
        constraint = user_token_account.mint == config.mint @ KriptoKError::InvalidMint
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub trader: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct LockRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, RoundAccount>,
    /// CHECK: Pyth Solana Price Feed
    pub pyth_price_feed: AccountInfo<'info>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct SettleRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, RoundAccount>,
    /// CHECK: Pyth Solana Price Feed Account
    pub pyth_price_feed: AccountInfo<'info>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct ClaimWinnings<'info> {
    #[account(
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, RoundAccount>,
    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, ProtocolConfig>,
    #[account(
        mut,
        seeds = [b"position", round_id.to_le_bytes().as_ref(), trader.key().as_ref()],
        bump = position.bump,
        constraint = position.trader == trader.key() @ KriptoKError::UnauthorizedAccount
    )]
    pub position: Account<'info, PositionAccount>,
    #[account(
        mut,
        seeds = [b"vault", round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = user_token_account.owner == trader.key() @ KriptoKError::UnauthorizedAccount,
        constraint = user_token_account.mint == config.mint @ KriptoKError::InvalidMint
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = treasury_token_account.key() == config.treasury @ KriptoKError::InvalidTreasury
    )]
    pub treasury_token_account: Account<'info, TokenAccount>,
    pub trader: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

// -----------------------------------------------------------------------------
// State Definitions
// -----------------------------------------------------------------------------
#[account]
pub struct ProtocolConfig {
    pub admin: Pubkey,
    pub treasury: Pubkey,
    pub mint: Pubkey,
    pub fee_basis_points: u16,
    pub min_stake: u64,
    pub max_stake: u64,
    pub bump: u8,
}

impl ProtocolConfig {
    pub const LEN: usize = 32 + 32 + 32 + 2 + 8 + 8 + 1;
}

#[account]
pub struct RoundAccount {
    pub round_id: u64,
    pub creator: Pubkey,
    pub start_time: i64,
    pub lock_time: i64,
    pub settle_time: i64,
    pub strike_price: i64,
    pub resolution_price: i64,
    pub total_bull_amount: u64,
    pub total_bear_amount: u64,
    pub status: RoundStatus,
    pub bump: u8,
    pub vault_bump: u8,
}

impl RoundAccount {
    pub const LEN: usize = 8 + 32 + 8 + 8 + 8 + 8 + 8 + 8 + 8 + 1 + 1 + 1;
}

#[account]
pub struct PositionAccount {
    pub round_id: u64,
    pub trader: Pubkey,
    pub direction: Direction,
    pub amount: u64,
    pub claimed: bool,
    pub bump: u8,
}

impl PositionAccount {
    pub const LEN: usize = 8 + 32 + 1 + 8 + 1 + 1;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Bull,
    Bear,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundStatus {
    Open,
    Locked,
    SettledBullWon,
    SettledBearWon,
    SettledDraw,
}

// -----------------------------------------------------------------------------
// Events
// -----------------------------------------------------------------------------
#[event]
pub struct ProtocolInitializedEvent {
    pub admin: Pubkey,
    pub treasury: Pubkey,
    pub fee_basis_points: u16,
}

#[event]
pub struct RoundCreatedEvent {
    pub round_id: u64,
    pub start_time: i64,
    pub lock_time: i64,
    pub settle_time: i64,
}

#[event]
pub struct PositionEnteredEvent {
    pub round_id: u64,
    pub trader: Pubkey,
    pub direction: Direction,
    pub amount: u64,
}

#[event]
pub struct RoundLockedEvent {
    pub round_id: u64,
    pub strike_price: i64,
    pub timestamp: i64,
}

#[event]
pub struct RoundSettledEvent {
    pub round_id: u64,
    pub strike_price: i64,
    pub resolution_price: i64,
    pub status: RoundStatus,
}

#[event]
pub struct WinningsClaimedEvent {
    pub round_id: u64,
    pub trader: Pubkey,
    pub payout_amount: u64,
    pub fee_deduction: u64,
}

// -----------------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------------
#[error_code]
pub enum KriptoKError {
    #[msg("Fee basis points exceed maximum allowed limit (1000 bps = 10%)")]
    ExcessiveFeeBps,
    #[msg("Invalid stake range specified")]
    InvalidStakeLimits,
    #[msg("Round timestamps are non-sequential or in the past")]
    InvalidTimestamps,
    #[msg("Round is not accepting entries in its current status")]
    RoundNotOpen,
    #[msg("Round lock time has already been reached")]
    RoundLocked,
    #[msg("Stake amount is below the configured protocol minimum")]
    StakeTooSmall,
    #[msg("Stake amount exceeds the configured protocol maximum")]
    StakeTooLarge,
    #[msg("Lock time has not been reached yet")]
    LockTimeNotReached,
    #[msg("Settlement time has already passed")]
    SettlementTimePassed,
    #[msg("Settlement time has not been reached yet")]
    SettlementTimeNotReached,
    #[msg("Round is not in Locked status")]
    RoundNotLocked,
    #[msg("Round has not settled yet")]
    RoundNotSettled,
    #[msg("Position has already been claimed")]
    AlreadyClaimed,
    #[msg("No winnings available for this position")]
    NoWinningsToClaim,
    #[msg("Invalid Pyth oracle account")]
    InvalidOracleAccount,
    #[msg("Pyth oracle price feed is stale or unavailable")]
    StaleOraclePrice,
    #[msg("Pyth oracle price confidence interval is wider than permitted tolerance")]
    PythConfidenceTooWide,
    #[msg("Pyth price reported non-positive value")]
    InvalidOraclePrice,
    #[msg("Signer is not authorized to act on this account")]
    UnauthorizedAccount,
    #[msg("Token account mint mismatch")]
    InvalidMint,
    #[msg("Treasury destination mismatch")]
    InvalidTreasury,
    #[msg("Arithmetic overflow during reward or fee calculation")]
    MathOverflow,
}