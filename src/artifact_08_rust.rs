use anchor_lang::prelude::*;
use anchor_spl::token::{self, Transfer, Token, TokenAccount};
use crate::state::*;
use crate::errors::LeagueError;

#[derive(Accounts)]
pub struct EnterAndPosition<'info> {
    #[account(
        mut,
        seeds = [b"round", round.round_id.to_le_bytes().as_ref()],
        bump = round.round_bump
    )]
    pub round: Account<'info, LeagueRound>,
    #[account(
        init,
        payer = trader,
        space = Position::LEN,
        seeds = [b"position", round.round_id.to_le_bytes().as_ref(), trader.key().as_ref()],
        bump
    )]
    pub position: Account<'info, Position>,
    #[account(
        mut,
        seeds = [b"vault", round.round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = user_token_account.owner == trader.key()
    )]
    pub user_token_account: Account<'info, TokenAccount>,
    #[account(mut)]
    pub trader: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<EnterAndPosition>,
    direction: TradeDirection,
    leverage: u8,
) -> Result<()> {
    let clock = Clock::get()?;
    let round = &mut ctx.accounts.round;

    require!(round.status == RoundStatus::Open, LeagueError::RoundLocked);
    require!(clock.unix_timestamp < round.lock_time, LeagueError::RoundLocked);
    require!(leverage >= 1 && leverage <= 10, LeagueError::MathOverflow);

    // Transfer collateral to vault PDA
    let cpi_ctx = CpiContext::new(
        ctx.accounts.token_program.to_account_info(),
        Transfer {
            from: ctx.accounts.user_token_account.to_account_info(),
            to: ctx.accounts.vault.to_account_info(),
            authority: ctx.accounts.trader.to_account_info(),
        },
    );
    token::transfer(cpi_ctx, round.entry_fee)?;

    round.total_pool = round.total_pool.checked_add(round.entry_fee).ok_or(LeagueError::MathOverflow)?;
    round.participant_count = round.participant_count.checked_add(1).ok_or(LeagueError::MathOverflow)?;

    let pos = &mut ctx.accounts.position;
    pos.trader = ctx.accounts.trader.key();
    pos.round_id = round.round_id;
    pos.direction = match direction {
        TradeDirection::Long => 0,
        TradeDirection::Short => 1,
    };
    pos.leverage = leverage;
    pos.score = 0;
    pos.claimed = false;
    pos.rank = 0;
    pos.bump = ctx.bumps.position;

    Ok(())
}