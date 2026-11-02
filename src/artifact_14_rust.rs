// 1. Reentrancy & Duplicate Claim Mitigation
// state.rs / claim_payout.rs
require!(!ticket.claimed, KriptoKError::AlreadyClaimed);
ticket.claimed = true; // State is mutated BEFORE CPI execution (Checks-Effects-Interactions)

// 2. Canonical Bump Derivation
// instructions/enter_trade.rs
seeds = [b"ticket", round_state.key().as_ref(), user.key().as_ref()],
bump // Anchor enforces canonical PDA bump checking implicitly

// 3. Oracle Boundary Assertion
// instructions/settle_round.rs
require!(
    price.conf <= (price.price as u64 / 200), // Maximum 0.50% confidence spread allowed
    KriptoKError::OracleConfidenceTooWide
);
require!(
    clock.unix_timestamp - price.publish_time <= league.oracle_max_staleness,
    KriptoKError::StaleOraclePrice
);