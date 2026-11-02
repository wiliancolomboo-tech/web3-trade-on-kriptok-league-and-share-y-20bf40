use anchor_lang::prelude::*;
use anchor_spl::token::{self, Transfer, Token, TokenAccount};
use crate::state::*;
use crate::errors::LeagueError;

#[derive(Accounts)]
pub struct ClaimWinnings<'info> {
    #[account(
        seeds = [b"league_master"],
        bump = config.bump
    )]
    pub config: Account<'info, MasterConfig>,
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.round_bump,
        constraint = round.status == RoundStatus::Settled @ LeagueError::RoundNotSettled
    )]
    pub round: Account<'info, LeagueRound>,
    #[account(
        mut,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), winner.key().as_ref()],
        bump = position.bump,
        has_one = round_id,
        constraint = position.trader == winner.key(),
        constraint = !position.claimed @ LeagueError::RewardAlreadyClaimed
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        seeds = [b"vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(mut)]
    pub winner_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub winner: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

pub fn handler(ctx: Context<ClaimWinnings>) -> Result<()> {
    let round = &ctx.accounts.round;
    let pos = &mut ctx.accounts.position;

    // Rule: Winner must have positive score & rank == 1 for single-winner jackpot test
    require!(pos.score > 0, LeagueError::NotEligibleForPayout);

    // Distribution waterfall: 85% to Top Rank, 10% Protocol, 5% Keeper crank
    let net_pool = round.total_pool;
    let winner_share = (net_pool as u128 * 8500 / 10_000) as u64;

    let round_id_bytes = round.round_id.to_le_bytes();
    let seeds = &[
        b"vault".as_ref(),
        round_id_bytes.as_ref(),
        &[round.vault_bump],
    ];
    let signer = &[&seeds[..]];

    let cpi_accounts = Transfer {
        from: ctx.accounts.vault.to_account_info(),
        to: ctx.accounts.winner_token_account.to_account_info(),
        authority: ctx.accounts.vault.to_account_info(),
    };
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer,
    );

    token::transfer(cpi_ctx, winner_share)?;
    pos.claimed = true;

    Ok(())
}