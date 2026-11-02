use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;

declare_id!("KrkLegue11111111111111111111111111111111111");

pub const MAX_PRICE_AGE_SECONDS: u64 = 60;
pub const BASIS_POINTS_DIVISOR: u64 = 10_000;

#[program]
pub mod kriptok_league {
    use super::*;

    /// Initialize a new competition league.
    pub fn initialize_league(
        ctx: Context<InitializeLeague>,
        league_id: u64,
        fee_basis_points: u16,
    ) -> Result<()> {
        require!(
            fee_basis_points <= 1000,
            LeagueError::ExcessiveProtocolFee
        ); // Max 10%

        let league = &mut ctx.accounts.league;
        league.admin = ctx.accounts.admin.key();
        league.collateral_mint = ctx.accounts.collateral_mint.key();
        league.league_id = league_id;
        league.round_counter = 0;
        league.fee_basis_points = fee_basis_points;
        league.total_volume = 0;
        league.bump = ctx.bumps.league;

        emit!(LeagueInitializedEvent {
            league_id,
            admin: league.admin,
            collateral_mint: league.collateral_mint,
            fee_basis_points,
        });

        Ok(())
    }

    /// Start a new competitive trading round linked to a target Pyth price oracle.
    pub fn start_round(
        ctx: Context<StartRound>,
        round_duration_seconds: i64,
        lock_duration_seconds: i64,
    ) -> Result<()> {
        require!(
            lock_duration_seconds < round_duration_seconds,
            LeagueError::InvalidRoundDurations
        );

        let clock = Clock::get()?;
        let current_timestamp = clock.unix_timestamp;

        // Fetch current baseline price from Pyth oracle feed
        let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
            .map_err(|_| LeagueError::InvalidOraclePrice)?;
        let current_price_data = price_feed
            .get_price_no_older_than(current_timestamp, MAX_PRICE_AGE_SECONDS)
            .ok_or(LeagueError::StaleOraclePrice)?;

        let league = &mut ctx.accounts.league;
        league.round_counter = league.round_counter.checked_add(1).unwrap();
        let round_id = league.round_counter;

        let round = &mut ctx.accounts.round;
        round.league_id = league.league_id;
        round.round_id = round_id;
        round.start_time = current_timestamp;
        round.lock_time = current_timestamp.checked_add(lock_duration_seconds).unwrap();
        round.end_time = current_timestamp.checked_add(round_duration_seconds).unwrap();
        round.pyth_oracle = ctx.accounts.pyth_price_feed.key();
        round.start_price = current_price_data.price;
        round.price_exponent = current_price_data.expo;
        round.final_price = 0;
        round.total_long_amount = 0;
        round.total_short_amount = 0;
        round.state = RoundState::Open;
        round.outcome = RoundOutcome::Undecided;
        round.bump = ctx.bumps.round;
        round.vault_bump = ctx.bumps.round_vault;

        emit!(RoundStartedEvent {
            league_id: league.league_id,
            round_id,
            start_price: round.start_price,
            lock_time: round.lock_time,
            end_time: round.end_time,
        });

        Ok(())
    }

    /// Trader deposits collateral and bets Long (Bull) or Short (Bear).
    pub fn enter_position(
        ctx: Context<EnterPosition>,
        direction: PositionDirection,
        amount: u64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let current_timestamp = clock.unix_timestamp;
        let round = &mut ctx.accounts.round;

        require!(round.state == RoundState::Open, LeagueError::RoundNotOpen);
        require!(
            current_timestamp < round.lock_time,
            LeagueError::RoundTradingLocked
        );
        require!(amount > 0, LeagueError::ZeroDepositAmount);

        // Execute CPI Token transfer from user to round vault
        let cpi_accounts = Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.round_vault.to_account_info(),
            authority: ctx.accounts.user.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Update Round stats
        match direction {
            PositionDirection::Long => {
                round.total_long_amount = round.total_long_amount.checked_add(amount).unwrap();
            }
            PositionDirection::Short => {
                round.total_short_amount = round.total_short_amount.checked_add(amount).unwrap();
            }
        }

        let league = &mut ctx.accounts.league;
        league.total_volume = league.total_volume.checked_add(amount).unwrap();

        // Update Position state
        let position = &mut ctx.accounts.position;
        position.user = ctx.accounts.user.key();
        position.round_id = round.round_id;
        position.direction = direction;
        position.amount = position.amount.checked_add(amount).unwrap();
        position.claimed = false;
        position.bump = ctx.bumps.position;

        emit!(PositionEnteredEvent {
            league_id: round.league_id,
            round_id: round.round_id,
            user: position.user,
            direction,
            amount,
        });

        Ok(())
    }

    /// Transition round state from Open to Locked once cutoff timestamp passes.
    pub fn lock_round(ctx: Context<LockRound>) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;

        require!(round.state == RoundState::Open, LeagueError::InvalidRoundState);
        require!(
            clock.unix_timestamp >= round.lock_time,
            LeagueError::LockTimeNotReached
        );

        round.state = RoundState::Locked;

        emit!(RoundLockedEvent {
            round_id: round.round_id,
            locked_at: clock.unix_timestamp,
        });

        Ok(())
    }

    /// Resolve round using Pyth Oracle closing price.
    pub fn resolve_round(ctx: Context<ResolveRound>) -> Result<()> {
        let clock = Clock::get()?;
        let current_timestamp = clock.unix_timestamp;
        let round = &mut ctx.accounts.round;

        require!(
            round.state == RoundState::Locked || round.state == RoundState::Open,
            LeagueError::InvalidRoundState
        );
        require!(
            current_timestamp >= round.end_time,
            LeagueError::RoundEndTimeNotReached
        );
        require!(
            ctx.accounts.pyth_price_feed.key() == round.pyth_oracle,
            LeagueError::OracleMismatch
        );

        let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
            .map_err(|_| LeagueError::InvalidOraclePrice)?;
        let final_price_data = price_feed
            .get_price_no_older_than(current_timestamp, MAX_PRICE_AGE_SECONDS)
            .ok_or(LeagueError::StaleOraclePrice)?;

        round.final_price = final_price_data.price;

        if round.final_price > round.start_price {
            round.outcome = RoundOutcome::LongWon;
        } else if round.final_price < round.start_price {
            round.outcome = RoundOutcome::ShortWon;
        } else {
            round.outcome = RoundOutcome::Draw;
        }

        round.state = RoundState::Resolved;

        emit!(RoundResolvedEvent {
            round_id: round.round_id,
            start_price: round.start_price,
            final_price: round.final_price,
            outcome: round.outcome,
        });

        Ok(())
    }

    /// Claim payout based on proportional pool weight.
    pub fn claim_reward(ctx: Context<ClaimReward>) -> Result<()> {
        let round = &ctx.accounts.round;
        let position = &mut ctx.accounts.position;

        require!(round.state == RoundState::Resolved, LeagueError::RoundNotResolved);
        require!(!position.claimed, LeagueError::RewardAlreadyClaimed);
        require!(position.amount > 0, LeagueError::ZeroDepositAmount);

        let total_pool = round
            .total_long_amount
            .checked_add(round.total_short_amount)
            .unwrap();

        let payout_amount: u64 = match round.outcome {
            RoundOutcome::LongWon => {
                require!(
                    position.direction == PositionDirection::Long,
                    LeagueError::PositionLost
                );
                // User share = (user_long_amount / total_long_amount) * total_pool * (1 - fee)
                compute_payout(
                    position.amount,
                    round.total_long_amount,
                    total_pool,
                    ctx.accounts.league.fee_basis_points,
                )?
            }
            RoundOutcome::ShortWon => {
                require!(
                    position.direction == PositionDirection::Short,
                    LeagueError::PositionLost
                );
                // User share = (user_short_amount / total_short_amount) * total_pool * (1 - fee)
                compute_payout(
                    position.amount,
                    round.total_short_amount,
                    total_pool,
                    ctx.accounts.league.fee_basis_points,
                )?
            }
            RoundOutcome::Draw => {
                // Return exact principal if oracle resolves to equal price
                position.amount
            }
            RoundOutcome::Undecided => return Err(LeagueError::RoundNotResolved.into()),
        };

        position.claimed = true;

        // CPI transfer from Vault PDA to user token account using PDA signer seeds
        let round_id_bytes = round.round_id.to_le_bytes();
        let seeds = &[
            b"vault".as_ref(),
            round_id_bytes.as_ref(),
            &[round.vault_bump],
        ];
        let signer_seeds = &[&seeds[..]];

        let cpi_accounts = Transfer {
            from: ctx.accounts.round_vault.to_account_info(),
            to: ctx.accounts.user_token_account.to_account_info(),
            authority: ctx.accounts.round_vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, payout_amount)?;

        emit!(RewardClaimedEvent {
            round_id: round.round_id,
            user: position.user,
            payout_amount,
        });

        Ok(())
    }
}

/// Helper math function: calculates net payout with protocol fee deduction.
fn compute_payout(
    user_stake: u64,
    winning_side_pool: u64,
    total_pool: u64,
    fee_bps: u16,
) -> Result<u64> {
    if winning_side_pool == 0 {
        return Ok(0);
    }
    // Net pool = total_pool * (10000 - fee_bps) / 10000
    let fee_factor = (BASIS_POINTS_DIVISOR as u128)
        .checked_sub(fee_bps as u128)
        .ok_or(LeagueError::MathOverflow)?;
    let net_pool = (total_pool as u128)
        .checked_mul(fee_factor)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(BASIS_POINTS_DIVISOR as u128)
        .ok_or(LeagueError::MathOverflow)?;

    // payout = (user_stake * net_pool) / winning_side_pool
    let payout = (user_stake as u128)
        .checked_mul(net_pool)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(winning_side_pool as u128)
        .ok_or(LeagueError::MathOverflow)?;

    Ok(payout as u64)
}

// -----------------------------------------------------------------------------
// ACCOUNT CONTEXTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(league_id: u64)]
pub struct InitializeLeague<'info> {
    #[account(
        init,
        payer = admin,
        space = 8 + League::INIT_SPACE,
        seeds = [b"league", league_id.to_le_bytes().as_ref()],
        bump
    )]
    pub league: Account<'info, League>,
    pub collateral_mint: Account<'info, Mint>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct StartRound<'info> {
    #[account(
        mut,
        seeds = [b"league", league.league_id.to_le_bytes().as_ref()],
        bump = league.bump
    )]
    pub league: Account<'info, League>,

    #[account(
        init,
        payer = authority,
        space = 8 + Round::INIT_SPACE,
        seeds = [
            b"round",
            league.league_id.to_le_bytes().as_ref(),
            (league.round_counter + 1).to_le_bytes().as_ref()
        ],
        bump
    )]
    pub round: Account<'info, Round>,

    #[account(
        init,
        payer = authority,
        seeds = [b"vault", (league.round_counter + 1).to_le_bytes().as_ref()],
        bump,
        token::mint = collateral_mint,
        token::authority = round_vault
    )]
    pub round_vault: Account<'info, TokenAccount>,

    pub collateral_mint: Account<'info, Mint>,

    /// CHECK: Pyth Oracle account verified by Pyth SDK parser
    pub pyth_price_feed: AccountInfo<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct EnterPosition<'info> {
    #[account(
        mut,
        seeds = [b"league", league.league_id.to_le_bytes().as_ref()],
        bump = league.bump
    )]
    pub league: Account<'info, League>,

    #[account(
        mut,
        seeds = [
            b"round",
            league.league_id.to_le_bytes().as_ref(),
            round.round_id.to_le_bytes().as_ref()
        ],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,

    #[account(
        init_if_needed,
        payer = user,
        space = 8 + Position::INIT_SPACE,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), user.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,

    #[account(
        mut,
        seeds = [b"vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_token_account.owner == user.key(),
        constraint = user_token_account.mint == league.collateral_mint
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
        seeds = [
            b"round",
            round.league_id.to_le_bytes().as_ref(),
            round.round_id.to_le_bytes().as_ref()
        ],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
}

#[derive(Accounts)]
pub struct ResolveRound<'info> {
    #[account(
        mut,
        seeds = [
            b"round",
            round.league_id.to_le_bytes().as_ref(),
            round.round_id.to_le_bytes().as_ref()
        ],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,

    /// CHECK: Pyth Oracle feed validated in instruction logic against round.pyth_oracle
    pub pyth_price_feed: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct ClaimReward<'info> {
    #[account(
        seeds = [b"league", round.league_id.to_le_bytes().as_ref()],
        bump = league.bump
    )]
    pub league: Account<'info, League>,

    #[account(
        seeds = [
            b"round",
            round.league_id.to_le_bytes().as_ref(),
            round.round_id.to_le_bytes().as_ref()
        ],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,

    #[account(
        mut,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), user.key().as_ref()],
        bump = position.bump,
        has_one = user
    )]
    pub position: Account<'info, Position>,

    #[account(
        mut,
        seeds = [b"vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = user_token_account.owner == user.key(),
        constraint = user_token_account.mint == league.collateral_mint
    )]
    pub user_token_account: Account<'info, TokenAccount>,

    pub user: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

// -----------------------------------------------------------------------------
// STATE STRUCTURES
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct League {
    pub admin: Pubkey,
    pub collateral_mint: Pubkey,
    pub league_id: u64,
    pub round_counter: u64,
    pub total_volume: u64,
    pub fee_basis_points: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Round {
    pub league_id: u64,
    pub round_id: u64,
    pub start_time: i64,
    pub lock_time: i64,
    pub end_time: i64,
    pub pyth_oracle: Pubkey,
    pub start_price: i64,
    pub final_price: i64,
    pub total_long_amount: u64,
    pub total_short_amount: u64,
    pub price_exponent: i32,
    pub state: RoundState,
    pub outcome: RoundOutcome,
    pub bump: u8,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub user: Pubkey,
    pub round_id: u64,
    pub amount: u64,
    pub direction: PositionDirection,
    pub claimed: bool,
    pub bump: u8,
}

// -----------------------------------------------------------------------------
// ENUMS & EVENTS
// -----------------------------------------------------------------------------

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum RoundState {
    Open,
    Locked,
    Resolved,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum RoundOutcome {
    Undecided,
    LongWon,
    ShortWon,
    Draw,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PositionDirection {
    Long,
    Short,
}

#[error_code]
pub enum LeagueError {
    #[msg("Protocol fee exceeds maximum allowed basis points (1000 bps = 10%)")]
    ExcessiveProtocolFee,
    #[msg("Invalid round durations: lock duration must be less than round duration")]
    InvalidRoundDurations,
    #[msg("Unable to load or parse Pyth price feed")]
    InvalidOraclePrice,
    #[msg("Pyth price feed timestamp exceeds maximum permissible staleness")]
    StaleOraclePrice,
    #[msg("The trading window for this round is currently closed")]
    RoundNotOpen,
    #[msg("Trading entry is locked for this epoch")]
    RoundTradingLocked,
    #[msg("Collateral deposit must be strictly greater than 0")]
    ZeroDepositAmount,
    #[msg("Invalid round state for requested operation")]
    InvalidRoundState,
    #[msg("Lock timestamp has not yet elapsed")]
    LockTimeNotReached,
    #[msg("Round duration end timestamp has not yet elapsed")]
    RoundEndTimeNotReached,
    #[msg("Oracle account does not match registered round oracle")]
    OracleMismatch,
    #[msg("Round has not yet been resolved")]
    RoundNotResolved,
    #[msg("Rewards for this position have already been claimed")]
    RewardAlreadyClaimed,
    #[msg("Trader placed bet on losing side")]
    PositionLost,
    #[msg("Mathematical overflow or arithmetic error occurred")]
    MathOverflow,
}

#[event]
pub struct LeagueInitializedEvent {
    pub league_id: u64,
    pub admin: Pubkey,
    pub collateral_mint: Pubkey,
    pub fee_basis_points: u16,
}

#[event]
pub struct RoundStartedEvent {
    pub league_id: u64,
    pub round_id: u64,
    pub start_price: i64,
    pub lock_time: i64,
    pub end_time: i64,
}

#[event]
pub struct PositionEnteredEvent {
    pub league_id: u64,
    pub round_id: u64,
    pub user: Pubkey,
    pub direction: PositionDirection,
    pub amount: u64,
}

#[event]
pub struct RoundLockedEvent {
    pub round_id: u64,
    pub locked_at: i64,
}

#[event]
pub struct RoundResolvedEvent {
    pub round_id: u64,
    pub start_price: i64,
    pub final_price: i64,
    pub outcome: RoundOutcome,
}

#[event]
pub struct RewardClaimedEvent {
    pub round_id: u64,
    pub user: Pubkey,
    pub payout_amount: u64,
}