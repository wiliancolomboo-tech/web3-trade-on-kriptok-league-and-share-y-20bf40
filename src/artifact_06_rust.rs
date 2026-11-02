use anchor_lang::prelude::*;

#[error_code]
pub enum LeagueError {
    #[msg("Calculation produced arithmetic overflow.")]
    MathOverflow,
    #[msg("Oracle price timestamp exceeds maximum permissible staleness.")]
    StaleOraclePrice,
    #[msg("Pyth oracle price returned non-positive or corrupted value.")]
    InvalidOraclePrice,
    #[msg("Oracle confidence interval is too wide for safe settlement.")]
    OracleConfidenceTooWide,
    #[msg("Round is not open for registration.")]
    RoundNotOpen,
    #[msg("Registration window has closed; round is locked.")]
    RoundLocked,
    #[msg("Round duration has not yet expired.")]
    RoundNotExpired,
    #[msg("Round is not yet settled.")]
    RoundNotSettled,
    #[msg("User position has already been scored.")]
    PositionAlreadyScored,
    #[msg("User position reward has already been claimed.")]
    RewardAlreadyClaimed,
    #[msg("Trader is not eligible for a prize payout.")]
    NotEligibleForPayout,
    #[msg("Unauthorized signer for keeper crank.")]
    UnauthorizedKeeper,
}