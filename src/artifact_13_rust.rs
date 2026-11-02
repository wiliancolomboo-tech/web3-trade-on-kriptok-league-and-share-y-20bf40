use anchor_lang::prelude::*;
use crate::errors::LeagueError;
use crate::events::PayoutClaimedEvent;
use crate::state::{LeagueConfig, Round, RoundOutcome, RoundStatus, TradeSide, UserPosition};

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct ClaimPayout<'info> {
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

    #[account(
        mut,
        seeds = [b"position", round_id.to_le_bytes().as_ref(), user.key().as_ref()],
        bump = position.position_bump,
        has_one = user @ LeagueError::Unauthorized
    )]
    pub position: Account<'info, UserPosition>,

    /// CHECK: Round Escrow Vault PDA
    #[account(
        mut,
        seeds = [b"vault", round_id.to_le_bytes().as_ref()],
        bump = round.vault_bump
    )]
    pub vault: SystemAccount<'info>,

    /// CHECK: Protocol Treasury recipient
    #[account(
        mut,
        address = config.treasury @ LeagueError::Unauthorized
    )]
    pub treasury: SystemAccount<'info>,

    #[account(mut)]
    pub user: Signer<'info>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<ClaimPayout>, round_id: u64) -> Result<()> {
    let round = &ctx.accounts.round;
    let position = &mut ctx.accounts.position;

    require!(round.status == RoundStatus::Resolved, LeagueError::RoundNotResolved);
    require!(!position.claimed, LeagueError::PositionAlreadyClaimed);

    let total_collateral = round
        .total_long_collateral
        .checked_add(round.total_short_collateral)
        .ok_or(LeagueError::MathOverflow)?;

    let net_pool = total_collateral
        .checked_sub(round.protocol_fee_collected)
        .ok_or(LeagueError::MathOverflow)?;

    let payout_amount: u64;

    match round.outcome {
        RoundOutcome::LongWon => {
            require!(position.side == TradeSide::Long, LeagueError::PositionDidNotWin);
            payout_amount = (position.amount as u128)
                .checked_mul(net_pool as u128)
                .ok_or(LeagueError::MathOverflow)?
                .checked_div(round.total_long_collateral as u128)
                .ok_or(LeagueError::MathOverflow)? as u64;
        }
        RoundOutcome::ShortWon => {
            require!(position.side == TradeSide::Short, LeagueError::PositionDidNotWin);
            payout_amount = (position.amount as u128)
                .checked_mul(net_pool as u128)
                .ok_or(LeagueError::MathOverflow)?
                .checked_div(round.total_short_collateral as u128)
                .ok_or(LeagueError::MathOverflow)? as u64;
        }
        RoundOutcome::Draw => {
            // In Draw: refund user deposit minus proportional protocol fee
            payout_amount = (position.amount as u128)
                .checked_mul(net_pool as u128)
                .ok_or(LeagueError::MathOverflow)?
                .checked_div(total_collateral as u128)
                .ok_or(LeagueError::MathOverflow)? as u64;
        }
        RoundOutcome::Undecided => return Err(LeagueError::RoundNotResolved.into()),
    }

    position.claimed = true;

    // Disburse funds from Vault PDA to User
    let round_id_bytes = round_id.to_le_bytes();
    let vault_seeds = &[
        b"vault".as_ref(),
        round_id_bytes.as_ref(),
        &[round.vault_bump],
    ];
    let signer_seeds = &[&vault_seeds[..]];

    **ctx.accounts.vault.to_account_info().try_borrow_mut_lamports()? = ctx
        .accounts
        .vault
        .to_account_info()
        .lamports()
        .checked_sub(payout_amount)
        .ok_or(LeagueError::MathOverflow)?;

    **ctx.accounts.user.to_account_info().try_borrow_mut_lamports()? = ctx
        .accounts
        .user
        .to_account_info()
        .lamports()
        .checked_add(payout_amount)
        .ok_or(LeagueError::MathOverflow)?;

    emit!(PayoutClaimedEvent {
        round_id,
        user: ctx.accounts.user.key(),
        payout_amount,
        timestamp: Clock::get()?.unix_timestamp,
    });

    Ok(())
}