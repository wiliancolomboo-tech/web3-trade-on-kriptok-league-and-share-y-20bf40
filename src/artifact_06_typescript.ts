// keeper_bot.ts
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "./target/types/kriptok_league";

async function runKeeper() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  console.log("Listening for rounds eligible for Lock/Resolve...");

  setInterval(async () => {
    const rounds = await program.account.leagueRound.all();
    const now = Math.floor(Date.now() / 1000);

    for (const round of rounds) {
      if (round.account.status.open && now >= round.account.lockTime.toNumber()) {
        console.log(`Locking round price for Round: ${round.account.roundId.toString()}`);
        await program.methods
          .lockRoundPrice()
          .accounts({
            round: round.publicKey,
            pythFeed: round.account.pythPriceFeed,
          })
          .rpc();
      } else if (round.account.status.locked && now >= round.account.resolveTime.toNumber()) {
        console.log(`Resolving round: ${round.account.roundId.toString()}`);
        // Trigger resolve_round and collect keeper incentive
      }
    }
  }, 2000);
}

runKeeper().catch(console.error);