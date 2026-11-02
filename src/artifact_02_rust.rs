use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use pyth_sdk_solana::load_price_feed_from_account_info;

declare_id!("kLeagU3X8SqpZ5QZz9gN9rV1wK9G1Yc7PmMv2r8X8ab");

pub const MAX_CONF_RATIO_BPS: u64 = 20; // 0.2% max confidence threshold
pub const MAX_PRICE_AGE_SECS: u64 = 30; // Max 30 seconds stale
pub const BPS_DENOMINATOR: u64 = 10_000;
pub const MAX_LEVERAGE: u64 = 10; // Up to 10x leverage

#[program]
pub mod kriptok_league {
    use super::*;

    pub fn initialize_league(
        ctx: Context<InitializeLeague>,
        fee_bps: u16,
        crank_reward: u64,
    ) -> Result<()> {
        require!(fee_bps <= 1000, LeagueError::FeeExceedsLimit); // Max 10%
        let league = &mut ctx.accounts.league;
        league.authority = ctx.accounts.authority.key();
        league.vault_mint = ctx.accounts.vault_mint.key();
        league.fee_recipient = ctx.accounts.fee_recipient.key();
        league.fee_bps = fee_bps;
        league.crank_reward = crank_reward;
        league.round_counter = 0;
        league.bump = ctx.bumps.league;
        Ok(())
    }

    pub fn create_round(
        ctx: Context<CreateRound>,
        round_duration_secs: i64,
        lock_offset_secs: i64,
        min_collateral: u64,
    ) -> Result<()> {
        require!(round_duration_secs > 0, LeagueError::InvalidDuration);
        require!(lock_offset_secs < round_duration_secs, LeagueError::InvalidLockOffset);
        require!(min_collateral > 0, LeagueError::ZeroCollateral);

        let clock = Clock::get()?;
        let league = &mut ctx.accounts.league;
        league.round_counter = league.round_counter.checked_add(1).ok_or(LeagueError::MathOverflow)?;

        let round = &mut ctx.accounts.round;
        round.round_id = league.round_counter;
        round.creator = ctx.accounts.creator.key();
        round.start_time = clock.unix_timestamp;
        round.lock_time = clock.unix_timestamp.checked_add(lock_offset_secs).ok_or(LeagueError::MathOverflow)?;
        round.end_time = clock.unix_timestamp.checked_add(round_duration_secs).ok_or(LeagueError::MathOverflow)?;
        round.min_collateral = min_collateral;
        round.total_collateral = 0;
        round.state = RoundState::Open;
        round.entry_price = 0;
        round.settlement_price = 0;
        round.participant_count = 0;
        round.bump = ctx.bumps.round;
        round.vault_bump = ctx.bumps.round_vault;

        Ok(())
    }

    pub fn enter_round(
        ctx: Context<EnterRound>,
        collateral_amount: u64,
        direction: PositionDirection,
        leverage: u64,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;

        require!(round.state == RoundState::Open, LeagueError::RoundNotOpen);
        require!(clock.unix_timestamp < round.lock_time, LeagueError::RoundLocked);
        require!(collateral_amount >= round.min_collateral, LeagueError::CollateralBelowMinimum);
        require!(leverage >= 1 && leverage <= MAX_LEVERAGE, LeagueError::InvalidLeverage);

        // Ingest Pyth Price as current benchmark
        let pyth_price_info = &ctx.accounts.pyth_feed;
        let price_feed = load_price_feed_from_account_info(pyth_price_info)
            .map_err(|_| LeagueError::InvalidPythFeed)?;
        let current_price = price_feed
            .get_price_no_older_than(clock.unix_timestamp, MAX_PRICE_AGE_SECS)
            .ok_or(LeagueError::StalePrice)?;

        require!(current_price.price > 0, LeagueError::InvalidPrice);
        let u_price = current_price.price as u64;
        let u_conf = current_price.conf;
        let conf_ratio = (u_conf as u128)
            .checked_mul(BPS_DENOMINATOR as u128)
            .ok_or(LeagueError::MathOverflow)?
            .checked_div(u_price as u128)
            .ok_or(LeagueError::MathOverflow)? as u64;
        require!(conf_ratio <= MAX_CONF_RATIO_BPS, LeagueError::PythConfidenceTooWide);

        // Snapshot initial benchmark price if first user
        if round.entry_price == 0 {
            round.entry_price = u_price;
        }

        // Transfer collateral from participant to vault PDA
        let cpi_accounts = Transfer {
            from: ctx.accounts.participant_token.to_account_info(),
            to: ctx.accounts.round_vault.to_account_info(),
            authority: ctx.accounts.participant.to_account_info(),
        };
        let cpi_program = ctx.accounts.token_program.to_account_info();
        let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);
        token::transfer(cpi_ctx, collateral_amount)?;

        // Record User Position
        let position = &mut ctx.accounts.position;
        position.round_id = round.round_id;
        position.owner = ctx.accounts.participant.key();
        position.collateral = collateral_amount;
        position.direction = direction;
        position.leverage = leverage;
        position.entry_price = u_price;
        position.claimed = false;
        position.bump = ctx.bumps.position;

        round.total_collateral = round.total_collateral.checked_add(collateral_amount).ok_or(LeagueError::MathOverflow)?;
        round.participant_count = round.participant_count.checked_add(1).ok_or(LeagueError::MathOverflow)?;

        Ok(())
    }

    pub fn settle_round(ctx: Context<SettleRound>) -> Result<()> {
        let clock = Clock::get()?;
        let round = &mut ctx.accounts.round;

        require!(round.state == RoundState::Open || round.state == RoundState::Locked, LeagueError::AlreadySettled);
        require!(clock.unix_timestamp >= round.end_time, LeagueError::RoundNotEnded);

        // Evaluate settlement price using Pyth
        let pyth_price_info = &ctx.accounts.pyth_feed;
        let price_feed = load_price_feed_from_account_info(pyth_price_info)
            .map_err(|_| LeagueError::InvalidPythFeed)?;
        let final_price = price_feed
            .get_price_no_older_than(clock.unix_timestamp, MAX_PRICE_AGE_SECS)
            .ok_or(LeagueError::StalePrice)?;

        require!(final_price.price > 0, LeagueError::InvalidPrice);
        round.settlement_price = final_price.price as u64;
        round.state = RoundState::Settled;

        emit!(RoundSettledEvent {
            round_id: round.round_id,
            settlement_price: round.settlement_price,
            timestamp: clock.unix_timestamp
        });

        Ok(())
    }

    pub fn claim_reward(ctx: Context<ClaimReward>) -> Result<()> {
        let round = &ctx.accounts.round;
        let position = &mut ctx.accounts.position;

        require!(round.state == RoundState::Settled, LeagueError::RoundNotSettled);
        require!(!position.claimed, LeagueError::AlreadyClaimed);
        require!(position.owner == ctx.accounts.participant.key(), LeagueError::Unauthorized);

        // Compute Directional PnL
        let entry = round.entry_price;
        let exit = round.settlement_price;
        let is_long = position.direction == PositionDirection::Long;

        let won = if is_long { exit > entry } else { exit < entry };
        let mut payout = 0u64;

        if won {
            let price_diff = if is_long {
                exit.checked_sub(entry).unwrap_or(0)
            } else {
                entry.checked_sub(exit).unwrap_or(0)
            };

            // Delta % = (diff / entry) * leverage
            let yield_scaled = (price_diff as u128)
                .checked_mul(position.leverage as u128).ok_or(LeagueError::MathOverflow)?
                .checked_mul(BPS_DENOMINATOR as u128).ok_or(LeagueError::MathOverflow)?
                .checked_div(entry as u128).ok_or(LeagueError::MathOverflow)?;

            // Profit = collateral * yield_scaled / BPS_DENOMINATOR
            let profit = (position.collateral as u128)
                .checked_mul(yield_scaled).ok_or(LeagueError::MathOverflow)?
                .checked_div(BPS_DENOMINATOR as u128).ok_or(LeagueError::MathOverflow)? as u64;

            payout = position.collateral.checked_add(profit).ok_or(LeagueError::MathOverflow)?;
        } else {
            // Under-performing position gets partial collateral return or liquidated if delta >= 100%
            let price_diff = if is_long {
                entry.checked_sub(exit).unwrap_or(0)
            } else {
                exit.checked_sub(entry).unwrap_or(0)
            };

            let loss_scaled = (price_diff as u128)
                .checked_mul(position.leverage as u128).ok_or(LeagueError::MathOverflow)?
                .checked_mul(BPS_DENOMINATOR as u128).ok_or(LeagueError::MathOverflow)?
                .checked_div(entry as u128).ok_or(LeagueError::MathOverflow)? as u64;

            if loss_scaled < BPS_DENOMINATOR {
                let loss = (position.collateral as u128)
                    .checked_mul(loss_scaled as u128).ok_or(LeagueError::MathOverflow)?
                    .checked_div(BPS_DENOMINATOR as u128).ok_or(LeagueError::MathOverflow)? as u64;
                payout = position.collateral.saturating_sub(loss);
            } else {
                payout = 0; // Total liquidation
            }
        }

        position.claimed = true;

        if payout > 0 {
            // Enforce vault safety ceiling
            let current_vault_balance = ctx.accounts.round_vault.amount;
            let final_transfer = std::cmp::min(payout, current_vault_balance);

            let round_id_bytes = round.round_id.to_le_bytes();
            let seeds = &[
                b"round_vault".as_ref(),
                round_id_bytes.as_ref(),
                &[round.vault_bump],
            ];
            let signer_seeds = &[&seeds[..]];

            let cpi_accounts = Transfer {
                from: ctx.accounts.round_vault.to_account_info(),
                to: ctx.accounts.participant_token.to_account_info(),
                authority: ctx.accounts.round_vault.to_account_info(),
            };
            let cpi_program = ctx.accounts.token_program.to_account_info();
            let cpi_ctx = CpiContext::new_with_signer(cpi_program, cpi_accounts, signer_seeds);
            token::transfer(cpi_ctx, final_transfer)?;
        }

        emit!(ClaimProcessedEvent {
            round_id: round.round_id,
            trader: position.owner,
            payout,
            won,
        });

        Ok(())
    }
}

// -----------------------------------------------------------------------------
// ACCOUNT VALIDATION CONTEXTS
// -----------------------------------------------------------------------------

#[derive(Accounts)]
pub struct InitializeLeague<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + LeagueConfig::INIT_SPACE,
        seeds = [b"league_config"],
        bump
    )]
    pub league: Account<'info, LeagueConfig>,
    pub vault_mint: Account<'info, Mint>,
    /// CHECK: Safe protocol fee recipient
    pub fee_recipient: AccountInfo<'info>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CreateRound<'info> {
    #[account(
        mut,
        seeds = [b"league_config"],
        bump = league.bump
    )]
    pub league: Account<'info, LeagueConfig>,
    #[account(
        init,
        payer = creator,
        space = 8 + Round::INIT_SPACE,
        seeds = [b"round", (league.round_counter + 1).to_le_bytes().as_ref()],
        bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        init,
        payer = creator,
        seeds = [b"round_vault", (league.round_counter + 1).to_le_bytes().as_ref()],
        bump,
        token::mint = vault_mint,
        token::authority = round_vault
    )]
    pub round_vault: Account<'info, TokenAccount>,
    pub vault_mint: Account<'info, Mint>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
pub struct EnterRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        mut,
        seeds = [b"round_vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,
    #[account(
        init,
        payer = participant,
        space = 8 + Position::INIT_SPACE,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), participant.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        constraint = participant_token.mint == round_vault.mint @ LeagueError::InvalidMint,
        constraint = participant_token.owner == participant.key() @ LeagueError::UnauthorizedTokenAccount
    )]
    pub participant_token: Account<'info, TokenAccount>,
    /// CHECK: Validated via Pyth SDK loader
    pub pyth_feed: AccountInfo<'info>,
    #[account(mut)]
    pub participant: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct SettleRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    /// CHECK: Validated via Pyth SDK loader
    pub pyth_feed: AccountInfo<'info>,
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct ClaimReward<'info> {
    #[account(
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.bump
    )]
    pub round: Account<'info, Round>,
    #[account(
        mut,
        seeds = [b"round_vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub round_vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), participant.key().as_ref()],
        bump = position.bump
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        constraint = participant_token.mint == round_vault.mint @ LeagueError::InvalidMint,
        constraint = participant_token.owner == participant.key() @ LeagueError::UnauthorizedTokenAccount
    )]
    pub participant_token: Account<'info, TokenAccount>,
    #[account(mut)]
    pub participant: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

// -----------------------------------------------------------------------------
// DATA STRUCTURES
// -----------------------------------------------------------------------------

#[account]
#[derive(InitSpace)]
pub struct LeagueConfig {
    pub authority: Pubkey,
    pub vault_mint: Pubkey,
    pub fee_recipient: Pubkey,
    pub crank_reward: u64,
    pub round_counter: u64,
    pub fee_bps: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Round {
    pub round_id: u64,
    pub creator: Pubkey,
    pub start_time: i64,
    pub lock_time: i64,
    pub end_time: i64,
    pub min_collateral: u64,
    pub total_collateral: u64,
    pub entry_price: u64,
    pub settlement_price: u64,
    pub participant_count: u32,
    pub state: RoundState,
    pub bump: u8,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Position {
    pub round_id: u64,
    pub owner: Pubkey,
    pub collateral: u64,
    pub entry_price: u64,
    pub leverage: u64,
    pub direction: PositionDirection,
    pub claimed: bool,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum RoundState {
    Open,
    Locked,
    Settled,
    Archived,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, InitSpace)]
pub enum PositionDirection {
    Long,
    Short,
}

// -----------------------------------------------------------------------------
// EVENTS & ERRORS
// -----------------------------------------------------------------------------

#[event]
pub struct RoundSettledEvent {
    pub round_id: u64,
    pub settlement_price: u64,
    pub timestamp: i64,
}

#[event]
pub struct ClaimProcessedEvent {
    pub round_id: u64,
    pub trader: Pubkey,
    pub payout: u64,
    pub won: bool,
}

#[error_code]
pub enum LeagueError {
    #[msg("Fee percentage exceeds allowed max limit (1000 bps)")]
    FeeExceedsLimit,
    #[msg("Invalid tournament duration")]
    InvalidDuration,
    #[msg("Invalid lock offset; must lock before end")]
    InvalidLockOffset,
    #[msg("Minimum collateral must be greater than zero")]
    ZeroCollateral,
    #[msg("Round is not accepting entries")]
    RoundNotOpen,
    #[msg("Round trading window is locked")]
    RoundLocked,
    #[msg("Collateral deposited is below round minimum")]
    CollateralBelowMinimum,
    #[msg("Leverage out of legal bounds (1x - 10x)")]
    InvalidLeverage,
    #[msg("Failed to parse Pyth Network price account")]
    InvalidPythFeed,
    #[msg("Pyth price observation is stale")]
    StalePrice,
    #[msg("Invalid Pyth absolute price reading")]
    InvalidPrice,
    #[msg("Pyth price confidence interval is too wide for safe settlement")]
    PythConfidenceTooWide,
    #[msg("Round has not reached its closing timestamp")]
    RoundNotEnded,
    #[msg("Round has already been settled")]
    AlreadySettled,
    #[msg("Round has not been settled yet")]
    RoundNotSettled,
    #[msg("Position payout already claimed")]
    AlreadyClaimed,
    #[msg("Unauthorized participant access")]
    Unauthorized,
    #[msg("Arithmetic overflow/underflow detected")]
    MathOverflow,
    #[msg("Token mint mismatch with round collateral vault")]
    InvalidMint,
    #[msg("Token account ownership mismatch")]
    UnauthorizedTokenAccount,
}