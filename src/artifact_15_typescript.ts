import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import { PublicKey, Keypair, SystemProgram, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { expect } from "chai";

describe("kriptok_league integration test suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  const admin = provider.wallet as anchor.Wallet;
  const treasury = Keypair.generate();
  const alice = Keypair.generate();
  const bob = Keypair.generate();

  let configPda: PublicKey;
  const roundId = new anchor.BN(101);
  let roundPda: PublicKey;
  let vaultPda: PublicKey;
  let alicePositionPda: PublicKey;
  let bobPositionPda: PublicKey;

  // Mock Pyth Price Feed account on Localnet
  const mockPythFeed = Keypair.generate();

  before(async () => {
    [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], program.programId);
    [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    [alicePositionPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), alice.publicKey.toBuffer()],
      program.programId
    );
    [bobPositionPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), bob.publicKey.toBuffer()],
      program.programId
    );

    // Fund test wallets
    const airdropAlice = await provider.connection.requestAirdrop(alice.publicKey, 10 * LAMPORTS_PER_SOL);
    const airdropBob = await provider.connection.requestAirdrop(bob.publicKey, 10 * LAMPORTS_PER_SOL);
    await provider.connection.confirmTransaction(airdropAlice);
    await provider.connection.confirmTransaction(airdropBob);
  });

  it("1. Initializes the KriptoK League Configuration", async () => {
    const feeBps = 200; // 2% protocol fee
    const roundDuration = new anchor.BN(4); // 4 seconds duration
    const lockLeadTime = new anchor.BN(2); // 2 seconds lock lead

    await program.methods
      .initializeLeague(feeBps, roundDuration, lockLeadTime)
      .accounts({
        config: configPda,
        treasury: treasury.publicKey,
        admin: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const configAccount = await program.account.leagueConfig.fetch(configPda);
    expect(configAccount.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(configAccount.feeBps).to.equal(200);
  });

  it("2. Spawns a new micro-trading round (Round #101)", async () => {
    await program.methods
      .startRound(roundId)
      .accounts({
        config: configPda,
        round: roundPda,
        vault: vaultPda,
        admin: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const roundData = await program.account.round.fetch(roundPda);
    expect(roundData.roundId.toNumber()).to.equal(101);
    expect(Object.keys(roundData.status)[0]).to.equal("open");
  });

  it("3. Places Long and Short positions from competing traders", async () => {
    const stakeAlice = new anchor.BN(2 * LAMPORTS_PER_SOL);
    const stakeBob = new anchor.BN(1 * LAMPORTS_PER_SOL);

    // Alice bets Long
    await program.methods
      .placeTrade(roundId, { long: {} }, stakeAlice)
      .accounts({
        round: roundPda,
        position: alicePositionPda,
        vault: vaultPda,
        user: alice.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([alice])
      .rpc();

    // Bob bets Short
    await program.methods
      .placeTrade(roundId, { short: {} }, stakeBob)
      .accounts({
        round: roundPda,
        position: bobPositionPda,
        vault: vaultPda,
        user: bob.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([bob])
      .rpc();

    const roundData = await program.account.round.fetch(roundPda);
    expect(roundData.totalLongCollateral.toString()).to.equal(stakeAlice.toString());
    expect(roundData.totalShortCollateral.toString()).to.equal(stakeBob.toString());
  });

  it("4. Prevents position modification after lock window expires", async () => {
    // Wait 2.5 seconds to pass lock_timestamp
    await new Promise((resolve) => setTimeout(resolve, 2500));

    const extraStake = new anchor.BN(1 * LAMPORTS_PER_SOL);
    try {
      await program.methods
        .placeTrade(roundId, { long: {} }, extraStake)
        .accounts({
          round: roundPda,
          position: alicePositionPda,
          vault: vaultPda,
          user: alice.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([alice])
        .rpc();
      expect.fail("Trade placement should have been rejected after lock expiration");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("TradingWindowClosed");
    }
  });
});