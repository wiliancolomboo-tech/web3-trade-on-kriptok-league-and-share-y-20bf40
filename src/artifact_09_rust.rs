use anchor_lang::prelude::*;
use crate::state::*;
use crate::errors::LeagueError;
use crate::math::{validate_and_extract_price, calculate_pnl_bps};
use pyth_sdk_solana::load_price_feed_from_account_info;

#[derive(Accounts)]
pub struct SettleRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.round_bump
    )]
    pub round: Account<'info, LeagueRound>,
    /// CHECK: Pyth Oracle feed
    pub pyth_price_feed: AccountInfo<'info>,
    pub keeper: Signer<'info>,
}

pub fn handler(ctx: Context<SettleRound>) -> Result<()> {
    let clock = Clock::get()?;
    let round = &mut ctx.accounts.round;

    require!(round.status == RoundStatus::Locked, LeagueError::RoundNotOpen);
    require!(clock.unix_timestamp >= round.end_time, LeagueError::RoundNotExpired);

    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| LeagueError::InvalidOraclePrice)?;
    let current_price = price_feed.get_price_no_older_than(clock.unix_timestamp, 60)
        .ok_or(LeagueError::StaleOraclePrice)?;

    let (price, expo) = validate_and_extract_price(&current_price, clock.unix_timestamp, 60)?;
    round.final_price = price;
    round.final_price_exp = expo;
    round.status = RoundStatus::Settled;

    Ok(())
}

#[derive(Accounts)]
pub struct ScoreParticipant<'info> {
    #[account(
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.round_bump,
        constraint = round.status == RoundStatus::Settled @ LeagueError::RoundNotSettled
    )]
    pub round: Account<'info, LeagueRound>,
    #[account(
        mut,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), position.trader.as_ref()],
        bump = position.bump,
        constraint = !position.claimed @ LeagueError::PositionAlreadyScored
    )]
    pub position: Account<'info, Position>,
    pub keeper: Signer<'info>,
}

pub fn score_handler(ctx: Context<ScoreParticipant>) -> Result<()> {
    let round = &ctx.accounts.round;
    let pos = &mut ctx.accounts.position;

    let is_long = pos.direction == 0;
    let score = calculate_pnl_bps(round.start_price, round.final_price, is_long, pos.leverage)?;
    pos.score = score;

    Ok(())
}