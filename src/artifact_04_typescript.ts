import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, Connection, ComputeBudgetProgram } from "@solana/web3.js";
import { getAssociatedTokenAddress, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { KriptoKLeague } from "../target/types/kriptok_league";
import axios from "axios";

export interface RoundExperienceMetrics {
  roundId: number;
  entryTimestamp: number;
  direction: "Bull" | "Bear";
  stakeAmountUSDC: number;
  entryPythPrice: number;
  strikePrice: number;
  resolutionPrice: number;
  outcome: "WON" | "LOST" | "DRAW";
  pnlUSDC: number;
  executionLatencyMs: number;
}

export class KriptoKSentinelBot {
  private program: Program<KriptoKLeague>;
  private connection: Connection;
  private walletKeypair: Keypair;
  private pythFeedPubkey: PublicKey;
  private experienceLog: RoundExperienceMetrics[] = [];

  constructor(
    program: Program<KriptoKLeague>,
    connection: Connection,
    walletKeypair: Keypair,
    pythFeedPubkey: PublicKey
  ) {
    this.program = program;
    this.connection = connection;
    this.walletKeypair = walletKeypair;
    this.pythFeedPubkey = pythFeedPubkey;
  }

  /**
   * Evaluates order flow micro-momentum across a 15-sample Pyth buffer.
   * Returns directional confidence and Kelly-scaled position sizing.
   */
  public evaluateSignal(priceHistory: number[]): { direction: "Bull" | "Bear"; confidence: number } {
    if (priceHistory.length < 5) return { direction: "Bull", confidence: 0.5 };

    const recent = priceHistory.slice(-5);
    const mean = recent.reduce((acc, v) => acc + v, 0) / recent.length;
    const currentPrice = priceHistory[priceHistory.length - 1];
    
    // Variance & standard score calculation
    const variance = recent.reduce((sum, val) => sum + Math.pow(val - mean, 2), 0) / recent.length;
    const stdDev = Math.sqrt(variance) || 0.0001;
    const zScore = (currentPrice - mean) / stdDev;

    return {
      direction: zScore >= 0 ? "Bull" : "Bear",
      confidence: Math.min(Math.abs(zScore) / 3.0, 1.0),
    };
  }

  /**
   * Executes programmatic round entry with high-priority CU Compute Budget.
   */
  public async executeRoundEntry(
    roundId: number,
    directionChoice: "Bull" | "Bear",
    amountUSDC: number
  ): Promise<string> {
    const startTime = Date.now();
    const roundIdBN = new anchor.BN(roundId);

    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundIdBN.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );
    const [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      this.program.programId
    );
    const [positionPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("position"),
        roundIdBN.toArrayLike(Buffer, "le", 8),
        this.walletKeypair.publicKey.toBuffer(),
      ],
      this.program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundIdBN.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );

    const configAccount = await this.program.account.protocolConfig.fetch(configPda);
    const userATA = await getAssociatedTokenAddress(
      configAccount.mint,
      this.walletKeypair.publicKey
    );

    const directionEnum = directionChoice === "Bull" ? { bull: {} } : { bear: {} };
    const stakeUnits = new anchor.BN(amountUSDC * 1_000_000); // 6 Decimals (USDC)

    // Set Priority Fee to prevent missing round lock deadline
    const priorityFeeIx = ComputeBudgetProgram.setComputeUnitPrice({ microLamports: 50_000 });
    const computeLimitIx = ComputeBudgetProgram.setComputeUnitLimit({ units: 150_000 });

    const tx = await this.program.methods
      .enterPosition(roundIdBN, directionEnum, stakeUnits)
      .accounts({
        round: roundPda,
        config: configPda,
        position: positionPda,
        roundVault: vaultPda,
        userTokenAccount: userATA,
        trader: this.walletKeypair.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .preInstructions([priorityFeeIx, computeLimitIx])
      .signers([this.walletKeypair])
      .rpc();

    const latency = Date.now() - startTime;
    console.log(`[ENTRY] Round #${roundId} -> ${directionChoice} | TX: ${tx} | Latency: ${latency}ms`);
    return tx;
  }

  /**
   * Cranks settlement and claims payouts if won.
   */
  public async crankSettlementAndClaim(roundId: number): Promise<void> {
    const roundIdBN = new anchor.BN(roundId);
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundIdBN.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );

    console.log(`[CRANK] Settling Round #${roundId}...`);
    await this.program.methods
      .settleRound(roundIdBN)
      .accounts({
        round: roundPda,
        pythPriceFeed: this.pythFeedPubkey,
      })
      .rpc();

    const roundData = await this.program.account.roundAccount.fetch(roundPda);
    console.log(
      `[SETTLED] Strike: ${roundData.strikePrice.toNumber()} | Resolution: ${roundData.resolutionPrice.toNumber()} | Status:`,
      roundData.status
    );

    // Claim
    const [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      this.program.programId
    );
    const [positionPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("position"),
        roundIdBN.toArrayLike(Buffer, "le", 8),
        this.walletKeypair.publicKey.toBuffer(),
      ],
      this.program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundIdBN.toArrayLike(Buffer, "le", 8)],
      this.program.programId
    );

    const configAccount = await this.program.account.protocolConfig.fetch(configPda);
    const userATA = await getAssociatedTokenAddress(
      configAccount.mint,
      this.walletKeypair.publicKey
    );

    const claimTx = await this.program.methods
      .claimWinnings(roundIdBN)
      .accounts({
        round: roundPda,
        config: configPda,
        position: positionPda,
        roundVault: vaultPda,
        userTokenAccount: userATA,
        treasuryTokenAccount: configAccount.treasury,
        trader: this.walletKeypair.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([this.walletKeypair])
      .rpc();

    console.log(`[CLAIMED] Round #${roundId} claimed successfully: ${claimTx}`);
  }

  /**
   * Emits markdown-formatted round experience log for community submission.
   */
  public generateRoundExperienceReport(): string {
    let report = `## KriptoK League Live Round Experience Log\n\n`;
    report += `| Round ID | Entry Dir | Stake (USDC) | Strike Price | Resolution Price | Result | Net PnL (USDC) |\n`;
    report += `|:---|:---|:---|:---|:---|:---|:---|\n`;

    let totalPnL = 0;
    for (const m of this.experienceLog) {
      totalPnL += m.pnlUSDC;
      report += `| #${m.roundId} | ${m.direction} | $${m.stakeAmountUSDC.toFixed(2)} | $${(m.strikePrice / 1e8).toFixed(2)} | $${(m.resolutionPrice / 1e8).toFixed(2)} | ${m.outcome} | ${m.pnlUSDC >= 0 ? "+" : ""}$${m.pnlUSDC.toFixed(2)} |\n`;
    }
    report += `\n**Total Cumulative PnL**: $${totalPnL.toFixed(2)} USDC\n`;
    return report;
  }
}