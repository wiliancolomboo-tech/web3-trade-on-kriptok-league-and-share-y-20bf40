import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import { Keypair, LAMPORTS_PER_SOL, PublicKey, SystemProgram } from "@solana/web3.js";
import { assert, expect } from "chai";
import * as crypto from "crypto";

describe("kriptok-league E2E Lifecycle", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;
  const admin = Keypair.generate();
  const trader1 = Keypair.generate();
  const trader2 = Keypair.generate();
  const mockPythOracle = Keypair.generate();

  let configPda: PublicKey;
  let roundPda: PublicKey;
  let prizeVaultPda: PublicKey;
  const roundId = new anchor.BN(101);
  const entryFee = new anchor.BN(0.25 * LAMPORTS_PER_SOL);

  before(async () => {
    // Fund Admin and Traders from local provider
    for (const kp of [admin, trader1, trader2]) {
      const sig = await provider.connection.requestAirdrop(kp.publicKey, 5 * LAMPORTS_PER_SOL);
      const latestBlockhash = await provider.connection.getLatestBlockhash();
      await provider.connection.confirmTransaction({ signature: sig, ...latestBlockhash });
    }

    [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league_config"), admin.publicKey.toBuffer()],
      program.programId
    );

    [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), configPda.toBuffer(), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    [prizeVaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("prize_vault"), roundPda.toBuffer()],
      program.programId
    );
  });

  it("Initializes a new KriptoK League competitive round", async () => {
    const now = Math.floor(Date.now() / 1000);
    const startTime = new anchor.BN(now - 10);
    const lockTime = new anchor.BN(now + 60);
    const endTime = new anchor.BN(now + 120);

    // Create a mock round
    await program.methods
      .initializeRound(roundId, startTime, lockTime, endTime, entryFee)
      .accounts({
        config: configPda,
        authority: admin.publicKey,
        round: roundPda,
        oracleFeed: mockPythOracle.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const roundData = await program.account.round.fetch(roundPda);
    assert.equal(roundData.roundId.toString(), roundId.toString());
    assert.deepEqual(roundData.status, { active: {} });
  });

  it("Traders join round and deposit collateral into PDA vault", async () => {
    const [p1State] = PublicKey.findProgramAddressSync(
      [Buffer.from("participant"), roundPda.toBuffer(), trader1.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .joinRound()
      .accounts({
        round: roundPda,
        participant: p1State,
        prizeVault: prizeVaultPda,
        user: trader1.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([trader1])
      .rpc();

    const vaultBalance = await provider.connection.getBalance(prizeVaultPda);
    assert.equal(vaultBalance, entryFee.toNumber());
  });

  it("Executes a directional Long trade and logs Round Experience hash", async () => {
    const [p1State] = PublicKey.findProgramAddressSync(
      [Buffer.from("participant"), roundPda.toBuffer(), trader1.publicKey.toBuffer()],
      program.programId
    );

    // Mock hash of review / trade thesis
    const experiencePayload = "Round 101: SOL breakout setup over $185 resistance. Seamless league UX.";
    const hash = crypto.createHash("sha256").update(experiencePayload).digest();

    const tx = await program.methods
      .executeTrade({ long: {} }, 5, Array.from(hash))
      .accounts({
        round: roundPda,
        participant: p1State,
        pythPriceFeed: mockPythOracle.publicKey,
        user: trader1.publicKey,
      })
      .signers([trader1])
      .rpc();

    expect(tx).to.be.a("string");
    const participantAccount = await program.account.participantState.fetch(p1State);
    assert.deepEqual(participantAccount.direction, { long: {} });
    assert.equal(participantAccount.leverage, 5);
  });
});