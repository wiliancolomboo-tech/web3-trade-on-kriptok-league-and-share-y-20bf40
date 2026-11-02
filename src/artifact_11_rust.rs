use anchor_lang::prelude::*;
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::errors::LeagueError;
use crate::events::RoundLockedEvent;
use crate::state::{Round, RoundStatus};

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct LockRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump = round.round_bump
    )]
    pub round: Account<'info, Round>,

    /// CHECK: Valid Pyth Price Feed account (e.g. SOL/USD)
    pub pyth_price_feed: AccountInfo<'info>,

    pub keeper: Signer<'info>,
}

pub fn handler(ctx: Context<LockRound>, round_id: u64) -> Result<()> {
    let round = &mut ctx.accounts.round;
    require!(round.status == RoundStatus::Open, LeagueError::RoundNotOpen);

    let now = Clock::get()?.unix_timestamp;
    require!(now >= round.lock_timestamp, LeagueError::LockTimeNotReached);

    // Pyth SDK Validation
    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| error!(LeagueError::OraclePriceStale))?;

    // Allow maximum 30 seconds age
    let current_price = price_feed
        .get_price_no_older_than(now, 30)
        .ok_or(LeagueError::OraclePriceStale)?;

    // Confidence interval sanity check: conf must be within 2% of price
    let max_allowed_conf = (current_price.price.abs() as u64)
        .checked_mul(2)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(100)
        .ok_or(LeagueError::MathOverflow)?;

    require!(current_price.conf <= max_allowed_conf, LeagueError::OracleConfidenceTooWide);

    round.entry_price = current_price.price;
    round.price_exponent = current_price.expo;
    round.status = RoundStatus::Locked;

    let total_pool = round
        .total_long_collateral
        .checked_add(round.total_short_collateral)
        .ok_or(LeagueError::MathOverflow)?;

    emit!(RoundLockedEvent {
        round_id,
        entry_price: current_price.price,
        price_exponent: current_price.expo,
        total_pool,
        timestamp: now,
    });

    Ok(())
}