// scripts/crank.ts
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";

async function runCrank() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  console.log("⚡ KriptoK Autonomous Crank Daemon Online...");

  setInterval(async () => {
    try {
      const rounds = await program.account.round.all();
      const now = Math.floor(Date.now() / 1000);

      for (const round of rounds) {
        if (
          round.account.status.hasOwnProperty("open") &&
          now >= round.account.lockTimestamp.toNumber()
        ) {
          console.log(`[LOCK TRIGGER] Locking Round #${round.account.roundId}`);
          await program.methods
            .lockRound()
            .accounts({
              round: round.publicKey,
              oracleFeed: round.account.oracleFeed,
              caller: provider.wallet.publicKey,
            })
            .rpc();
        }

        if (
          round.account.status.hasOwnProperty("locked") &&
          now >= round.account.closeTimestamp.toNumber()
        ) {
          console.log(`[RESOLVE TRIGGER] Settling Round #${round.account.roundId}`);
          // Execute round resolution instruction via RPC...
        }
      }
    } catch (err) {
      console.error("Crank iteration error:", err);
    }
  }, 1000);
}

runCrank().catch(console.error);