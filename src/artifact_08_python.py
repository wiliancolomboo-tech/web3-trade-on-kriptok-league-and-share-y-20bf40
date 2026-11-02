import time
import asyncio
from solana.rpc.async_api import AsyncClient
from solders.pubkey import Pubkey

RPC_URL = "https://api.devnet.solana.com"
PROGRAM_ID = Pubkey.from_string("KrpTk4Rk1Vf8eD6wX6e1hQy1q2L8yU7uW4uQ9Zk1234")

async def run_crank_loop():
    client = AsyncClient(RPC_URL)
    print(f"[*] KriptoK Crank Daemon started for Program: {PROGRAM_ID}")
    
    while True:
        try:
            # Query active round PDAs
            # If (current_time >= lock_time and status == Open) -> trigger lock_round
            # If (current_time >= end_time and status == Locked) -> trigger settle_round
            print(f"[+] Scanning active league rounds at {int(time.time())}...")
            await asyncio.sleep(5)
        except Exception as e:
            print(f"[-] Keeper error: {e}")
            await asyncio.sleep(5)

if __name__ == "__main__":
    asyncio.run(run_crank_loop())