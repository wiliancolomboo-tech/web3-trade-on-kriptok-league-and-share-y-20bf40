# 1. Set Solana CLI config to Devnet
solana config set --url https://api.devnet.solana.com

# 2. Fund deployer wallet
solana airdrop 2

# 3. Deploy to Solana Devnet
anchor deploy --provider.cluster devnet

# 4. Launch Autonomous Bot & Crank Worker
ts-node bot/kriptok_bot.ts --network devnet --market SOL-USD