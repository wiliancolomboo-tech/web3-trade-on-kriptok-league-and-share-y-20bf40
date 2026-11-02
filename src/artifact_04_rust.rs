use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;

declare_id!("KrpTk11111111111111111111111111111111111111");

pub const CONFIG_SEED: &[u8] = b"league_config";
pub const ROUND_SEED: &[u8] = b"round";
pub const POSITION_SEED: &[u8] = b"position";
pub const VAULT_SEED: &[u8] = b"vault";

pub const MAX_CONF_RATIO: u64 = 500; // max confidence interval = 5% of price
pub const MAX_ORACLE_STALENESS_SECONDS: u64 = 60; // 60s staleness guard
pub const BASIS_POINTS_DIVISOR: u64 = 10_000;

#[program]
pub mod kriptok_league {
    use super::*;

    pub fn initialize_league(
        ctx: Context<InitializeLeague>,
        protocol_fee_bps: u16,
        min_wager: u64,
        max_wager: u64,
    ) -> Result<()> {
        require!(protocol_fee_bps <= 1000, KriptoKError::FeeTooHigh); // max 10%
        require!(min_wager > 0 && max_wager >= min_wager, KriptoKError::InvalidWagerRange);

        let config = &mut ctx.accounts.config;
        config.admin = ctx.accounts.admin.key();
        config.collateral_mint = ctx.accounts.collateral_mint.key();
        config.treasury = ctx.accounts.treasury.key();
        config.protocol_fee_bps = protocol_fee_bps;
        config.min_wager = min_wager;
        config.max_wager = max_wager;
        config.total_rounds_counter = 0;
        config.bump = ctx.bumps.config;

        emit!(LeagueInitializedEvent {
            admin: config.admin,
            collateral_mint: config.collateral_mint,
            protocol_fee_bps,
        });
        Ok(())
    }

    pub fn start_round(
        ctx: Context<StartRound>,
        round_id: u64,
        duration_seconds: i64,
        oracle_price_feed: Pubkey,
    ) -> Result<()> {
        let config = &mut ctx.accounts.config;
        require!(round_id == config.total_rounds_counter + 1, KriptoKError::InvalidRoundId);
        require!(duration_seconds >= 30, KriptoKError::InvalidDuration); // min 30s round

        let now = Clock::get()?.unix_timestamp;
        let round = &mut ctx.accounts.round;

        round.round_id = round_id;
        round.state = RoundState::Open;
        round.oracle_feed = oracle_price_feed;
        round.created_at = now;
        round.lock_timestamp = now.checked_add(duration_seconds).ok_or(KriptoKError::MathOverflow)?;
        round.settle_timestamp = round.lock_timestamp.checked_add(duration_seconds).ok_or(KriptoKError::MathOverflow)?;
        round.strike_price = 0;
        round.settlement_price = 0;
        round.bull_pool = 0;
        round.bear_pool = 0;
        round.total_payout_pool = 0;
        round.fee_amount = 0;
        round.winner = RoundWinner::Undetermined;
        round.bump = ctx.bumps.round;
        round.vault_bump = ctx.bumps.vault;

        config.total_rounds_counter = round_id;

        emit!(RoundStartedEvent {
            round_id,
            lock_timestamp: round.lock_timestamp,
            settle_timestamp: round.settle_timestamp,
            oracle_feed: oracle_price_feed,
        });
        Ok(())
    }

    pub fn enter_position(
        ctx: Context<EnterPosition>,
        direction: Direction,
        amount: u64,
    ) -> Result<()> {
        let round = &mut ctx.accounts.round;
        let config = &ctx.accounts.config;
        let now = Clock::get()?.unix_timestamp;

        require!(round.state == RoundState::Open, KriptoKError::RoundNotOpen);
        require!(now < round.lock_timestamp, KriptoKError::BettingWindowClosed);
        require!(amount >= config.min_wager && amount <= config.max_wager, KriptoKError::WagerOutOfBounds);

        // Spl-Token CPI transfer to vault
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.vault.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        token::transfer(CpiContext::new(cpi_program, cpi_accounts), amount)?;

        // Record or update user position
        let position = &mut ctx.accounts.position;
        if position.amount == 0 {
            position.user = ctx.accounts.user.key();
            position.round_id = round.round_id;
            position.direction = direction;
            position.amount = amount;
            position.claimed = false;
            position.bump = ctx.bumps.position;
        } else {
            require!(position.direction == direction, KriptoKError::CannotSwitchDirection);
            position.amount = position.amount.checked_add(amount).ok_or(KriptoKError::MathOverflow)?;
        }

        match direction {
            Direction::Bull => {
                round.bull_pool = round.bull_pool.checked_add(amount).ok_or(KriptoKError::MathOverflow)?;
            }
            Direction::Bear => {
                round.bear_pool = round.bear_pool.checked_add(amount).ok_or(KriptoKError::MathOverflow)?;
            }
        }

        emit!(PositionEnteredEvent {
            round_id: round.round_id,
            user: ctx.accounts.user.key(),
            direction,
            amount,
        });
        Ok(())
    }

    pub fn lock_round(ctx: Context<LockRound>) -> Result<()> {
        let round = &mut ctx.accounts.round;
        let now = Clock::get()?.unix_timestamp;

        require!(round.state == RoundState::Open, KriptoKError::InvalidState);
        require!(now >= round.lock_timestamp, KriptoKError::LockTimeNotReached);

        // Fetch validated Pyth price
        let price = get_pyth_price(&ctx.accounts.oracle_feed, now)?;

        round.strike_price = price;
        round.state = RoundState::Locked;

        emit!(RoundLockedEvent {
            round_id: round.round_id,
            strike_price: price,
            locked_at: now,
        });
        Ok(())
    }

    pub fn settle_round(ctx: Context<SettleRound>) -> Result<()> {
        let round = &mut ctx.accounts.round;
        let config = &ctx.accounts.config;
        let now = Clock::get()?.unix_timestamp;

        require!(round.state == RoundState::Locked, KriptoKError::InvalidState);
        require!(now >= round.settle_timestamp, KriptoKError::SettleTimeNotReached);

        let final_price = get_pyth_price(&ctx.accounts.oracle_feed, now)?;
        round.settlement_price = final_price;

        let total_collateral = round.bull_pool.checked_add(round.bear_pool).ok_or(KriptoKError::MathOverflow)?;

        if round.bull_pool == 0 || round.bear_pool == 0 || final_price == round.strike_price {
            // Draw condition or one-sided wager: Full refund, no fees charged
            round.winner = RoundWinner::Draw;
            round.total_payout_pool = total_collateral;
            round.fee_amount = 0;
        } else {
            let fee = (total_collateral as u128)
                .checked_mul(config.protocol_fee_bps as u128)
                .ok_or(KriptoKError::MathOverflow)?
                .checked_div(BASIS_POINTS_DIVISOR as u128)
                .ok_or(KriptoKError::MathOverflow)? as u64;

            let net_payout = total_collateral.checked_sub(fee).ok_or(KriptoKError::MathOverflow)?;
            round.fee_amount = fee;
            round.total_payout_pool = net_payout;

            if final_price > round.strike_price {
                round.winner = RoundWinner::Bull;
            } else {
                round.winner = RoundWinner::Bear;
            }

            // Transfer protocol fee to treasury using vault signer seeds
            if fee > 0 {
                let round_id_bytes = round.round_id.to_le_bytes();
                let seeds: &[&[&[u8]]] = &[&[
                    VAULT_SEED,
                    round_id_bytes.as_ref(),
                    &[round.vault_bump],
                ]];

                let cpi_ctx = CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.vault.to_account_info(),
                        to: ctx.accounts.treasury.to_account_info(),
                        authority: ctx.accounts.vault.to_account_info(),
                    },
                    seeds,
                );
                token::transfer(cpi_ctx, fee)?;
            }
        }

        round.state = RoundState::Settled;

        emit!(RoundSettledEvent {
            round_id: round.round_id,
            strike_price: round.strike_price,
            settlement_price: final_price,
            winner: round.winner,
            total_payout: round.total_payout_pool,
            fee_collected: round.fee_amount,
        });
        Ok(())
    }

    pub fn claim_payout(ctx: Context<ClaimPayout>) -> Result<()> {
        let round = &ctx.accounts.round;
        let position = &mut ctx.accounts.position;

        require!(round.state == RoundState::Settled, KriptoKError::RoundNotSettled);
        require!(!position.claimed, KriptoKError::AlreadyClaimed);
        require!(position.amount > 0, KriptoKError::NoPositionFound);

        let payout_amount: u64;

        match round.winner {
            RoundWinner::Draw => {
                // Refund original amount
                payout_amount = position.amount;
            }
            RoundWinner::Bull => {
                require!(position.direction == Direction::Bull, KriptoKError::PositionNotWinning);
                payout_amount = (position.amount as u128)
                    .checked_mul(round.total_payout_pool as u128)
                    .ok_or(KriptoKError::MathOverflow)?
                    .checked_div(round.bull_pool as u128)
                    .ok_or(KriptoKError::MathOverflow)? as u64;
            }
            RoundWinner::Bear => {
                require!(position.direction == Direction::Bear, KriptoKError::PositionNotWinning);
                payout_amount = (position.amount as u128)
                    .checked_mul(round.total_payout_pool as u128)
                    .ok_or(KriptoKError::MathOverflow)?
                    .checked_div(round.bear_pool as u128)
                    .ok_or(KriptoKError::MathOverflow)? as u64;
            }
            RoundWinner::Undetermined => return err!(KriptoKError::RoundNotSettled),
        }

        position.claimed = true;

        let round_id_bytes = round.round_id.to_le_bytes();
        let seeds: &[&[&[u8]]] = &[&[
            VAULT_SEED,
            round_id_bytes.as_ref(),
            &[round.vault_bump],
        ]];

        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.vault.to_account_info(),
                to: ctx.accounts.user_token_account.to_account_info(),
                authority: ctx.accounts.vault.to_account_info(),
            },
            seeds,
        );
        token::transfer(cpi_ctx, payout_amount)?;

        emit!(PayoutClaimedEvent {
            round_id: round.round_id,
            user: ctx.accounts.user.key(),
            amount: payout_amount,
        });
        Ok(())
    }
}

// ---------------- Helper Functions ----------------

fn get_pyth_price(oracle_account: &AccountInfo, current_timestamp: i64) -> Result<i64> {
    let price_feed = load_price_feed_from_account_info(oracle_account)
        .map_err(|_| KriptoKError::InvalidPythFeed)?;

    let price_data = price_feed
        .get_price_no_older_than(current_timestamp, MAX_ORACLE_STALENESS_SECONDS)
        .ok_or(KriptoKError::OraclePriceStale)?;

    // Check price is positive
    require!(price_data.price > 0, KriptoKError::InvalidOraclePrice);

    // Confidence interval check
    let conf_interval = price_data.conf;
    let max_allowed_conf = (price_data.price as u64)
        .checked_mul(MAX_CONF_RATIO)
        .ok_or(KriptoKError::MathOverflow)?
        .checked_div(BASIS_POINTS_DIVISOR)
        .ok_or(KriptoKError::MathOverflow)?;

    require!(conf_interval <= max_allowed_conf, KriptoKError::OracleConfidenceTooLow);

    Ok(price_data.price)
}

// ---------------- Contexts & State Structs ----------------

#[derive(Accounts)]
pub struct InitializeLeague<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + LeagueConfig::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, LeagueConfig>,
    pub collateral_mint: Account<'info, Mint>,
    #[account(
        constraint = treasury.mint == collateral_mint.key() @ KriptoKError::InvalidTreasuryAccount
    )]
    pub treasury: Account<'info, TokenAccount>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct StartRound<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, LeagueConfig>,
    #[account(
        init,
        payer = authority,
        space = 8 + Round::INIT_SPACE,
        seeds = [ROUND_SEED, round_id.to_le_bytes().as_ref()],
        bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        init,
        payer = authority,
        seeds = [VAULT_SEED, round_id.to_le_bytes().as_ref()],
        bump,
        token::mint = collateral_mint,
        token::authority = vault
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(address = config.collateral_mint)]
    pub collateral_mint: Account<'info, Mint>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct EnterPosition<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, LeagueConfig>,
    #[account(
        mut,
        seeds = [ROUND_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        init_if_needed,
        payer = user,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, round.key().as_ref(), user.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        seeds = [VAULT_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = user_token_account.mint == config.collateral_mint @ KriptoKError::InvalidMint,
        constraint = user_token_account.owner == user.key() @ KriptoKError::UnauthorizedAccount
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct LockRound<'info> {
    #[account(
        mut,
        seeds = [ROUND_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    /// CHECK: Validated inside pyth reader helper
    #[account(address = round.oracle_feed)]
    pub oracle_feed: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct SettleRound<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, LeagueConfig>,
    #[account(
        mut,
        seeds = [ROUND_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        mut,
        seeds = [VAULT_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut, address = config.treasury)]
    pub treasury: Account<'info, TokenAccount>,
    /// CHECK: Validated via round.oracle_feed
    #[account(address = round.oracle_feed)]
    pub oracle_feed: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ClaimPayout<'info> {
    #[account(
        mut,
        seeds = [ROUND_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        mut,
        seeds = [POSITION_SEED, round.key().as_ref(), user.key().as_ref()],
        bump = position.bump,
        has_one = user @ KriptoKError::UnauthorizedAccount
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        seeds = [VAULT_SEED, round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = user_token_account.owner == user.key() @ KriptoKError::UnauthorizedAccount
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

// ---------------- Data Structures ----------------

#[account]
#[derive(InitSpace)]
pub struct LeagueConfig {
    pub admin: Pubkey,
    pub collateral_mint: Pubkey,
    pub treasury: Pubkey,
    pub min_wager: u64,
    pub max_wager: u64,
    pub total_rounds_counter: u64,
    pub protocol_fee_bps: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Round {
    pub round_id: u64,
    pub oracle_feed: Pubkey,
    pub created_at: i64,
    pub lock_timestamp: i64,
    pub settle_timestamp: i64,
    pub strike_price: i64,
    pub settlement_price: i64,
    pub bull_pool: u64,
    pub bear_pool: u64,
    pub total_payout_pool: u64,
    pub fee_amount: u64,
    pub state: RoundState,
    pub winner: RoundWinner,
    pub bump: u8,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub user: Pubkey,
    pub round_id: u64,
    pub direction: Direction,
    pub amount: u64,
    pub claimed: bool,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum Direction {
    Bull,
    Bear,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum RoundState {
    Open,
    Locked,
    Settled,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace, Debug)]
pub enum RoundWinner {
    Undetermined,
    Bull,
    Bear,
    Draw,
}

// ---------------- Events ----------------

#[event]
pub struct LeagueInitializedEvent {
    pub admin: Pubkey,
    pub collateral_mint: Pubkey,
    pub protocol_fee_bps: u16,
}

#[event]
pub struct RoundStartedEvent {
    pub round_id: u64,
    pub lock_timestamp: i64,
    pub settle_timestamp: i64,
    pub oracle_feed: Pubkey,
}

#[event]
pub struct PositionEnteredEvent {
    pub round_id: u64,
    pub user: Pubkey,
    pub direction: Direction,
    pub amount: u64,
}

#[event]
pub struct RoundLockedEvent {
    pub round_id: u64,
    pub strike_price: i64,
    pub locked_at: i64,
}

#[event]
pub struct RoundSettledEvent {
    pub round_id: u64,
    pub strike_price: i64,
    pub settlement_price: i64,
    pub winner: RoundWinner,
    pub total_payout: u64,
    pub fee_collected: u64,
}

#[event]
pub struct PayoutClaimedEvent {
    pub round_id: u64,
    pub user: Pubkey,
    pub amount: u64,
}

// ---------------- Errors ----------------

#[error_code]
pub enum KriptoKError {
    #[msg("Fee basis points cannot exceed 1000 (10%)")]
    FeeTooHigh,
    #[msg("Invalid wager range specification")]
    InvalidWagerRange,
    #[msg("Invalid round sequence ID")]
    InvalidRoundId,
    #[msg("Round duration must be at least 30 seconds")]
    InvalidDuration,
    #[msg("Round is not currently accepting entries")]
    RoundNotOpen,
    #[msg("Betting window has closed for this round")]
    BettingWindowClosed,
    #[msg("Wager amount violates minimum or maximum boundaries")]
    WagerOutOfBounds,
    #[msg("Cannot alternate bet direction within same round")]
    CannotSwitchDirection,
    #[msg("Invalid round state for requested operation")]
    InvalidState,
    #[msg("Lock timestamp has not yet been reached")]
    LockTimeNotReached,
    #[msg("Settlement timestamp has not yet been reached")]
    SettleTimeNotReached,
    #[msg("Failed to parse Pyth price feed")]
    InvalidPythFeed,
    #[msg("Pyth price feed is stale beyond threshold")]
    OraclePriceStale,
    #[msg("Pyth price confidence is wider than allowed variance")]
    OracleConfidenceTooLow,
    #[msg("Oracle price value must be strictly positive")]
    InvalidOraclePrice,
    #[msg("Arithmetic overflow or underflow encountered")]
    MathOverflow,
    #[msg("Round has not reached final settlement")]
    RoundNotSettled,
    #[msg("Winnings have already been claimed")]
    AlreadyClaimed,
    #[msg("No wager balance found to claim")]
    NoPositionFound,
    #[msg("User position is not on the winning outcome")]
    PositionNotWinning,
    #[msg("Token account mint mismatch")]
    InvalidMint,
    #[msg("Unauthorized account operation")]
    UnauthorizedAccount,
    #[msg("Supplied treasury token account does not match mint")]
    InvalidTreasuryAccount,
}