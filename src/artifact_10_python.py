import time
import asyncio
from solana.rpc.async_api import AsyncClient
from solders.pubkey import Pubkey

RPC_URL = "http://127.0.0.1:8899"
PROGRAM_ID = Pubkey.from_string("KrpTk11111111111111111111111111111111111111")

async def run_crank():
    client = AsyncClient(RPC_URL)
    print(f"[*] KriptoK Crank listening on {RPC_URL}...")
    while True:
        try:
            # Poll active rounds needing lock or settlement
            current_time = int(time.time())
            # Keeper logic: Fetch PDA accounts -> check status -> dispatch lock/settle
            await asyncio.sleep(2)
        except Exception as e:
            print(f"[!] Crank Error: {e}")
            await asyncio.sleep(5)

if __name__ == "__main__":
    asyncio.run(run_crank())