import * as anchor from "@coral-xyz/anchor";
import { Connection, Keypair, PublicKey, ComputeBudgetProgram } from "@solana/web3.js";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";

export class KriptoKLeagueKeeper {
  private program: Program<KriptokLeague>;
  private connection: Connection;
  private keeperKeypair: Keypair;
  private pythFeedPubkey: PublicKey;

  constructor(
    program: Program<KriptokLeague>,
    connection: Connection,
    keeperKeypair: Keypair,
    pythFeedPubkey: PublicKey
  ) {
    this.program = program;
    this.connection = connection;
    this.keeperKeypair = keeperKeypair;
    this.pythFeedPubkey = pythFeedPubkey;
  }

  public async crankRoundLifecycle(roundId: anchor.BN): Promise<void> {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );

    const roundData = await this.program.account.round.fetch(roundPda);
    const nowSec = Math.floor(Date.now() / 1000);

    const modifyComputeUnits = ComputeBudgetProgram.setComputeUnitLimit({ units: 150_000 });
    const addPriorityFee = ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 });

    if (Object.keys(roundData.status)[0] === "open" && nowSec >= roundData.lockTimestamp.toNumber()) {
      console.log(`[Keeper] Round ${roundId.toString()} lock window reached. Executing lock_round...`);
      const tx = await this.program.methods
        .lockRound(roundId)
        .accounts({
          round: roundPda,
          pythPriceFeed: this.pythFeedPubkey,
          keeper: this.keeperKeypair.publicKey,
        })
        .preInstructions([modifyComputeUnits, addPriorityFee])
        .signers([this.keeperKeypair])
        .rpc();

      console.log(`[Keeper] Successfully locked round ${roundId.toString()}. Tx: ${tx}`);
    } else if (
      Object.keys(roundData.status)[0] === "locked" &&
      nowSec >= roundData.resolveTimestamp.toNumber()
    ) {
      console.log(`[Keeper] Round ${roundId.toString()} resolution epoch reached. Executing resolve_round...`);
      const [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], this.program.programId);

      const tx = await this.program.methods
        .resolveRound(roundId)
        .accounts({
          config: configPda,
          round: roundPda,
          pythPriceFeed: this.pythFeedPubkey,
          keeper: this.keeperKeypair.publicKey,
        })
        .preInstructions([modifyComputeUnits, addPriorityFee])
        .signers([this.keeperKeypair])
        .rpc();

      console.log(`[Keeper] Successfully resolved round ${roundId.toString()}. Tx: ${tx}`);
    }
  }

  public startPolling(roundId: anchor.BN, intervalMs: number = 1000): NodeJS.Timeout {
    return setInterval(async () => {
      try {
        await this.crankRoundLifecycle(roundId);
      } catch (err) {
        console.error(`[Keeper Error] Polling cycle failed:`, err);
      }
    }, intervalMs);
  }
}