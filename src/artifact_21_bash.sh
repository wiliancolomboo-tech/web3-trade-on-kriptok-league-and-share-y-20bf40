# Switch config to Devnet
solana config set --url devnet

# Airdrop devnet SOL to deployment keypair
solana airdrop 2

# Sync program ID keys
anchor keys sync

# Build & Deploy to Devnet
anchor build
anchor deploy --provider.cluster devnet