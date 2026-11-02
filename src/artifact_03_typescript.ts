import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { PublicKey, Keypair, SystemProgram, SYSVAR_RENT_PUBKEY } from "@solana/web3.js";
import { assert, expect } from "chai";

describe("kriptok-league protocol test suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  const admin = Keypair.generate();
  const traderAlice = Keypair.generate();
  const traderBob = Keypair.generate();

  let usdcMint: PublicKey;
  let aliceUsdcAccount: PublicKey;
  let bobUsdcAccount: PublicKey;
  let feeRecipient: PublicKey;

  let leagueConfigPda: PublicKey;
  let round1Pda: PublicKey;
  let round1VaultPda: PublicKey;

  // Mock Pyth Account Public Key (pointing to devnet SOL/USD or mock fixture)
  const mockPythSolUsd = new PublicKey("H6ARHf6YXhGYeQfUzQNGk6rDNnLBQKrenN712K4SE5nv");

  before(async () => {
    // Fund Admin and Traders with native SOL for gas
    for (const kp of [admin, traderAlice, traderBob]) {
      const sig = await provider.connection.requestAirdrop(kp.publicKey, 10 * anchor.web3.LAMPORTS_PER_SOL);
      const latestBlockhash = await provider.connection.getLatestBlockhash();
      await provider.connection.confirmTransaction({ signature: sig, ...latestBlockhash });
    }

    // Initialize Mock Collateral Mint (USDC - 6 Decimals)
    usdcMint = await createMint(
      provider.connection,
      admin,
      admin.publicKey,
      null,
      6
    );

    feeRecipient = (await Keypair.generate()).publicKey;

    // Create and Mint Token Accounts
    aliceUsdcAccount = await createAccount(provider.connection, traderAlice, usdcMint, traderAlice.publicKey);
    bobUsdcAccount = await createAccount(provider.connection, traderBob, usdcMint, traderBob.publicKey);

    await mintTo(provider.connection, admin, usdcMint, aliceUsdcAccount, admin, 10_000 * 1e6); // $10,000
    await mintTo(provider.connection, admin, usdcMint, bobUsdcAccount, admin, 10_000 * 1e6); // $10,000

    [leagueConfigPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league_config")],
      program.programId
    );
  });

  it("1. Initializes League Protocol Global Config", async () => {
    const tx = await program.methods
      .initializeLeague(100, new anchor.BN(5 * 1e6)) // 1% fee, 5 USDC crank
      .accounts({
        league: leagueConfigPda,
        vaultMint: usdcMint,
        feeRecipient: feeRecipient,
        authority: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const config = await program.account.leagueConfig.fetch(leagueConfigPda);
    expect(config.authority.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(config.feeBps).to.equal(100);
    expect(config.roundCounter.toNumber()).to.equal(0);
  });

  it("2. Creates Round #1 (5-Second Sprint)", async () => {
    const nextRoundId = 1;
    const roundIdBuffer = new anchor.BN(nextRoundId).toArrayLike(Buffer, "le", 8);

    [round1Pda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundIdBuffer],
      program.programId
    );

    [round1VaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round_vault"), roundIdBuffer],
      program.programId
    );

    const tx = await program.methods
      .createRound(
        new anchor.BN(5), // 5 seconds duration
        new anchor.BN(2), // Lock at 2 seconds
        new anchor.BN(50 * 1e6) // 50 USDC min
      )
      .accounts({
        league: leagueConfigPda,
        round: round1Pda,
        roundVault: round1VaultPda,
        vaultMint: usdcMint,
        creator: admin.publicKey,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .signers([admin])
      .rpc();

    const roundState = await program.account.round.fetch(round1Pda);
    expect(roundState.roundId.toNumber()).to.equal(1);
    expect(roundState.totalCollateral.toNumber()).to.equal(0);
  });

  it("3. Alice Enters Round #1 with Long Position (5x Leverage)", async () => {
    const roundIdBuffer = new anchor.BN(1).toArrayLike(Buffer, "le", 8);
    const [alicePosPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundIdBuffer, traderAlice.publicKey.toBuffer()],
      program.programId
    );

    const depositAmount = new anchor.BN(500 * 1e6); // 500 USDC

    await program.methods
      .enterRound(
        depositAmount,
        { long: {} },
        new anchor.BN(5) // 5x
      )
      .accounts({
        round: round1Pda,
        roundVault: round1VaultPda,
        position: alicePosPda,
        participantToken: aliceUsdcAccount,
        pythFeed: mockPythSolUsd,
        participant: traderAlice.publicKey,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([traderAlice])
      .rpc();

    const pos = await program.account.position.fetch(alicePosPda);
    expect(pos.collateral.toNumber()).to.equal(500 * 1e6);
    expect(pos.leverage.toNumber()).to.equal(5);

    const vaultAccount = await getAccount(provider.connection, round1VaultPda);
    expect(Number(vaultAccount.amount)).to.equal(500 * 1e6);
  });

  it("4. Prevents Entry Past Lock Window", async () => {
    // Sleep 3 seconds to breach lock_time
    await new Promise((resolve) => setTimeout(resolve, 3000));

    const roundIdBuffer = new anchor.BN(1).toArrayLike(Buffer, "le", 8);
    const [bobPosPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundIdBuffer, traderBob.publicKey.toBuffer()],
      program.programId
    );

    try {
      await program.methods
        .enterRound(
          new anchor.BN(200 * 1e6),
          { short: {} },
          new anchor.BN(2)
        )
        .accounts({
          round: round1Pda,
          roundVault: round1VaultPda,
          position: bobPosPda,
          participantToken: bobUsdcAccount,
          pythFeed: mockPythSolUsd,
          participant: traderBob.publicKey,
          systemProgram: SystemProgram.programId,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([traderBob])
        .rpc();
      assert.fail("Should have thrown RoundLocked error");
    } catch (err: any) {
      expect(err.toString()).to.include("RoundLocked");
    }
  });

  it("5. Settles Round After Duration Expiration", async () => {
    // Wait until end_time is exceeded
    await new Promise((resolve) => setTimeout(resolve, 3500));

    await program.methods
      .settleRound()
      .accounts({
        round: round1Pda,
        pythFeed: mockPythSolUsd,
        caller: admin.publicKey,
      })
      .signers([admin])
      .rpc();

    const roundData = await program.account.round.fetch(round1Pda);
    expect(roundData.state).to.deep.equal({ settled: {} });
    expect(roundData.settlementPrice.toNumber()).to.be.greaterThan(0);
  });
});