# Set environment variables
export ANCHOR_PROVIDER_URL="https://api.devnet.solana.com"
export ANCHOR_WALLET="~/.config/solana/id.json"
export LEAGUE_ID="101"
# Pyth SOL/USD Devnet Feed Address:
export PYTH_ORACLE_FEED="J83w4HKfqxwcq3BEMMkPFSQdE2qUfgTC8qaxuFH8nd5v"

ts-node scripts/crank-keeper.ts