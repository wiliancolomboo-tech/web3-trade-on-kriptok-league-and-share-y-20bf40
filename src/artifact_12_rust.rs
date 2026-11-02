use anchor_lang::prelude::*;
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::errors::LeagueError;
use crate::events::RoundResolvedEvent;
use crate::state::{LeagueConfig, Round, RoundOutcome, RoundStatus};

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct ResolveRound<'info> {
    #[account(
        seeds = [b"config"],
        bump = config.bump
    )]
    pub config: Account<'info, LeagueConfig>,

    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.round_bump
    )]
    pub round: Account<'info, Round>,

    /// CHECK: Pyth Price Feed Account
    pub pyth_price_feed: AccountInfo<'info>,

    pub keeper: Signer<'info>,
}

pub fn handler(ctx: Context<ResolveRound>, round_id: u64) -> Result<()> {
    let round = &mut ctx.accounts.round;
    require!(round.status == RoundStatus::Locked, LeagueError::RoundNotLocked);

    let now = Clock::get()?.unix_timestamp;
    require!(now >= round.resolve_timestamp, LeagueError::ResolveTimeNotReached);

    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| error!(LeagueError::OraclePriceStale))?;

    let exit_price_struct = price_feed
        .get_price_no_older_than(now, 30)
        .ok_or(LeagueError::OraclePriceStale)?;

    round.exit_price = exit_price_struct.price;

    let outcome = if round.exit_price > round.entry_price {
        RoundOutcome::LongWon
    } else if round.exit_price < round.entry_price {
        RoundOutcome::ShortWon
    } else {
        RoundOutcome::Draw
    };

    round.outcome = outcome;
    round.status = RoundStatus::Resolved;

    let total_pool = round
        .total_long_collateral
        .checked_add(round.total_short_collateral)
        .ok_or(LeagueError::MathOverflow)?;

    let protocol_fee = total_pool
        .checked_mul(ctx.accounts.config.fee_bps as u64)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(10000)
        .ok_or(LeagueError::MathOverflow)?;

    round.protocol_fee_collected = protocol_fee;
    let net_winning_pool = total_pool.checked_sub(protocol_fee).ok_or(LeagueError::MathOverflow)?;

    emit!(RoundResolvedEvent {
        round_id,
        exit_price: round.exit_price,
        outcome,
        net_winning_pool,
        timestamp: now,
    });

    Ok(())
}