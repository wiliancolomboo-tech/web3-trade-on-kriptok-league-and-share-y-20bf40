import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair } from "@solana/web3.js";
import { KriptokLeague } from "../target/types/kriptok_league";

export class KriptoKLeagueKeeper {
  private program: Program<KriptokLeague>;
  private keeperKeypair: Keypair;
  private leagueId: anchor.BN;
  private pythFeed: PublicKey;

  constructor(
    program: Program<KriptokLeague>,
    keeperKeypair: Keypair,
    leagueId: number,
    pythFeed: PublicKey
  ) {
    this.program = program;
    this.keeperKeypair = keeperKeypair;
    this.leagueId = new anchor.BN(leagueId);
    this.pythFeed = pythFeed;
  }

  public async runLoop(pollIntervalMs = 2000) {
    console.log(`[Keeper] Starting crank loop for League ID: ${this.leagueId.toString()}`);

    setInterval(async () => {
      try {
        await this.checkAndProcessRounds();
      } catch (err) {
        console.error(`[Keeper Error] Loop iteration failed:`, err);
      }
    }, pollIntervalMs);
  }

  private async checkAndProcessRounds() {
    const [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), this.leagueId.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );

    const leagueAccount = await this.program.account.league.fetch(leaguePda);
    const activeRoundId = leagueAccount.roundCounter;

    if (activeRoundId.eqn(0)) return;

    const [roundPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("round"),
        this.leagueId.toArrayLike(Buffer, "le", 8),
        activeRoundId.toArrayLike(Buffer, "le", 8),
      ],
      this.program.programId
    );

    const roundAccount = await this.program.account.round.fetch(roundPda);
    const nowSec = Math.floor(Date.now() / 1000);

    // State 1: Open -> Need Lock?
    if (
      JSON.stringify(roundAccount.state) === JSON.stringify({ open: {} }) &&
      nowSec >= roundAccount.lockTime.toNumber()
    ) {
      console.log(`[Keeper] Locking Round ${activeRoundId.toString()}...`);
      const tx = await this.program.methods
        .lockRound()
        .accounts({ round: roundPda })
        .signers([this.keeperKeypair])
        .rpc();
      console.log(`[Keeper] Round locked in tx: ${tx}`);
    }

    // State 2: Locked -> Need Resolution?
    if (
      (JSON.stringify(roundAccount.state) === JSON.stringify({ locked: {} }) ||
        JSON.stringify(roundAccount.state) === JSON.stringify({ open: {} })) &&
      nowSec >= roundAccount.endTime.toNumber()
    ) {
      console.log(`[Keeper] Resolving Round ${activeRoundId.toString()}...`);
      const tx = await this.program.methods
        .resolveRound()
        .accounts({
          round: roundPda,
          pythPriceFeed: this.pythFeed,
        })
        .signers([this.keeperKeypair])
        .rpc();
      console.log(`[Keeper] Round successfully resolved: ${tx}`);
    }
  }
}