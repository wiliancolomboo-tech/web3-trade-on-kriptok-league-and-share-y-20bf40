use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};
use crate::state::*;
use crate::errors::LeagueError;
use pyth_sdk_solana::load_price_feed_from_account_info;
use crate::math::validate_and_extract_price;

#[derive(Accounts)]
pub struct InitializeMaster<'info> {
    #[account(
        init,
        payer = admin,
        space = MasterConfig::LEN,
        seeds = [b"league_master"],
        bump
    )]
    pub config: Account<'info, MasterConfig>,
    pub treasury: SystemAccount<'info>,
    #[account(mut)]
    pub admin: Signer<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<InitializeMaster>, protocol_fee_bps: u16, crank_fee_bps: u16) -> Result<()> {
    let cfg = &mut ctx.accounts.config;
    cfg.admin = ctx.accounts.admin.key();
    cfg.treasury = ctx.accounts.treasury.key();
    cfg.protocol_fee_bps = protocol_fee_bps;
    cfg.crank_fee_bps = crank_fee_bps;
    cfg.total_rounds_created = 0;
    cfg.bump = ctx.bumps.config;
    Ok(())
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct CreateRound<'info> {
    #[account(
        mut,
        seeds = [b"league_master"],
        bump = config.bump
    )]
    pub config: Account<'info, MasterConfig>,
    #[account(
        init,
        payer = creator,
        space = LeagueRound::LEN,
        seeds = [b"round", round_id.to_le_bytes().as_ref()],
        bump
    )]
    pub round: Account<'info, LeagueRound>,
    #[account(
        init,
        payer = creator,
        seeds = [b"vault", round_id.to_le_bytes().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = vault
    )]
    pub vault: Account<'info, TokenAccount>,
    pub usdc_mint: Account<'info, Mint>,
    #[account(mut)]
    pub creator: Signer<'info>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn handler(
    ctx: Context<CreateRound>,
    round_id: u64,
    entry_fee: u64,
    duration_seconds: i64,
) -> Result<()> {
    let clock = Clock::get()?;
    let round = &mut ctx.accounts.round;
    round.round_id = round_id;
    round.status = RoundStatus::Open;
    round.entry_fee = entry_fee;
    round.start_time = clock.unix_timestamp;
    round.lock_time = clock.unix_timestamp + 300; // 5 min join window
    round.end_time = round.lock_time + duration_seconds;
    round.total_pool = 0;
    round.participant_count = 0;
    round.start_price = 0;
    round.start_price_exp = 0;
    round.final_price = 0;
    round.final_price_exp = 0;
    round.vault_bump = ctx.bumps.vault;
    round.round_bump = ctx.bumps.round;

    let config = &mut ctx.accounts.config;
    config.total_rounds_created = config.total_rounds_created.checked_add(1).unwrap();
    Ok(())
}

#[derive(Accounts)]
pub struct LockRound<'info> {
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.round_bump
    )]
    pub round: Account<'info, LeagueRound>,
    /// CHECK: Pyth Oracle account validated via Pyth SDK loader
    pub pyth_price_feed: AccountInfo<'info>,
    pub keeper: Signer<'info>,
}

pub fn lock_round_handler(ctx: Context<LockRound>) -> Result<()> {
    let clock = Clock::get()?;
    let round = &mut ctx.accounts.round;
    require!(round.status == RoundStatus::Open, LeagueError::RoundLocked);
    require!(clock.unix_timestamp >= round.lock_time, LeagueError::RoundNotOpen);

    let price_feed = load_price_feed_from_account_info(&ctx.accounts.pyth_price_feed)
        .map_err(|_| LeagueError::InvalidOraclePrice)?;
    let current_price = price_feed.get_price_no_older_than(clock.unix_timestamp, 60)
        .ok_or(LeagueError::StaleOraclePrice)?;

    let (price, expo) = validate_and_extract_price(&current_price, clock.unix_timestamp, 60)?;
    round.start_price = price;
    round.start_price_exp = expo;
    round.status = RoundStatus::Locked;

    Ok(())
}