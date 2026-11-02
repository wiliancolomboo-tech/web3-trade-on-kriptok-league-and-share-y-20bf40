# Set environment variables
export ANCHOR_PROVIDER_URL="http://127.0.0.1:8899"
export ANCHOR_WALLET="$HOME/.config/solana/id.json"
export PYTH_FEED_ADDRESS="H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4SE5nv"

# Launch autonomous crank bot
npx ts-node keeper/keeper_bot.ts