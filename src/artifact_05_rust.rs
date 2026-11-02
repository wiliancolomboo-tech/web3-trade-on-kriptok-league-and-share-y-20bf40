use anchor_lang::prelude::*;
use crate::errors::LeagueError;
use pyth_sdk_solana::Price;

pub const BPS_DIVISOR: i64 = 10_000;
pub const MAX_ALLOWED_CONFIDENCE_RATIO: u64 = 100; // 1% confidence boundary

/// Validates Pyth price freshness and returns normalized 64-bit signed price
pub fn validate_and_extract_price(
    price_feed: &Price,
    current_timestamp: i64,
    max_staleness: i64,
) -> Result<(i64, i32)> {
    require!(price_feed.price > 0, LeagueError::InvalidOraclePrice);
    
    let age = current_timestamp
        .checked_sub(price_feed.publish_time)
        .ok_or(LeagueError::MathOverflow)?;
    require!(age <= max_staleness, LeagueError::StaleOraclePrice);

    // Confidence Interval verification: conf / price <= 1%
    let conf_ratio = (price_feed.conf as u128)
        .checked_mul(10_000)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(price_feed.price as u128)
        .ok_or(LeagueError::MathOverflow)?;

    require!(
        conf_ratio <= MAX_ALLOWED_CONFIDENCE_RATIO as u128,
        LeagueError::OracleConfidenceTooWide
    );

    Ok((price_feed.price, price_feed.expo))
}

/// Calculate trading score normalized to Basis Points
pub fn calculate_pnl_bps(
    start_price: i64,
    final_price: i64,
    direction_is_long: bool,
    leverage: u8,
) -> Result<i64> {
    require!(start_price > 0 && final_price > 0, LeagueError::InvalidOraclePrice);

    let price_delta = final_price
        .checked_sub(start_price)
        .ok_or(LeagueError::MathOverflow)?;

    let raw_pct_diff = (price_delta as i128)
        .checked_mul(BPS_DIVISOR as i128)
        .ok_or(LeagueError::MathOverflow)?
        .checked_div(start_price as i128)
        .ok_or(LeagueError::MathOverflow)?;

    let directed_bps = if direction_is_long {
        raw_pct_diff
    } else {
        -raw_pct_diff
    };

    let leveraged_score = directed_bps
        .checked_mul(leverage as i128)
        .ok_or(LeagueError::MathOverflow)?;

    Ok(leveraged_score as i64)
}