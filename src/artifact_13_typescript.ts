import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import {
  createMint,
  createAccount,
  mintTo,
  TOKEN_PROGRAM_ID
} from "@solana/spl-token";
import { expect } from "chai";

describe("KriptoK League - Full Lifecycle End-to-End Suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  let usdcMint: anchor.web3.Pubkey;
  let adminUsdcAta: anchor.web3.Pubkey;
  let mockPythPriceFeed: anchor.web3.Keypair;

  const roundDurationSlots = new anchor.BN(10);
  const entryFee = new anchor.BN(50 * 1_000_000); // 50 USDC
  const protocolFeeBps = 500; // 5%
  const currentRoundId = new anchor.BN(101);

  before(async () => {
    // Deploy Mock USDC Mint
    usdcMint = await createMint(
      provider.connection,
      admin,
      admin.publicKey,
      null,
      6
    );

    adminUsdcAta = await createAccount(
      provider.connection,
      admin,
      usdcMint,
      admin.publicKey
    );

    await mintTo(
      provider.connection,
      admin,
      usdcMint,
      adminUsdcAta,
      admin,
      10_000 * 1_000_000
    );

    // Initialize mock Pyth Price Feed Account
    mockPythPriceFeed = anchor.web3.Keypair.generate();
    const lamports = await provider.connection.getMinimumBalanceForRentExemption(3312);
    
    // Construct Pyth struct buffer mock
    const pythData = Buffer.alloc(3312);
    pythData.writeUInt32LE(0xa1b2c3d4, 0); // Magic
    pythData.writeUInt32LE(2, 4);          // Version
    pythData.writeInt32LE(-6, 20);        // Exponent (-6)
    pythData.writeBigInt64LE(BigInt(150_000_000), 208); // Mock Price $150.00
    pythData.writeBigUInt64LE(BigInt(100_000), 216);    // Mock Conf $0.10
    pythData.writeBigInt64LE(BigInt(Math.floor(Date.now() / 1000)), 224); // Publish Time

    const tx = new anchor.web3.Transaction().add(
      anchor.web3.SystemProgram.createAccount({
        fromPubkey: admin.publicKey,
        newAccountPubkey: mockPythPriceFeed.publicKey,
        lamports,
        space: 3312,
        programId: anchor.web3.SystemProgram.programId,
      })
    );
    await provider.sendAndConfirm(tx, [admin, mockPythPriceFeed]);
  });

  it("1. Initializes League Protocol Global Configurations", async () => {
    const [configPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );

    const [vaultPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("vault"), usdcMint.toBuffer()],
      program.programId
    );

    await program.methods
      .initializeLeague(roundDurationSlots, entryFee, protocolFeeBps)
      .accountsStrict({
        admin: admin.publicKey,
        config: configPda,
        usdcMint: usdcMint,
        vault: vaultPda,
        systemProgram: anchor.web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: anchor.web3.SysvarRent.programId,
      })
      .rpc();

    const config = await program.account.leagueConfig.fetch(configPda);
    expect(config.entryFeeAmount.toString()).to.equal(entryFee.toString());
    expect(config.protocolFeeBps).to.equal(500);
  });

  it("2. Trader joins round and transfers USDC entry fee to vault PDA", async () => {
    const [configPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );
    const [roundPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("round"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [positionPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("position"), currentRoundId.toArrayLike(Buffer, "le", 8), admin.publicKey.toBuffer()],
      program.programId
    );
    const [leaderboardPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("leaderboard"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("vault"), usdcMint.toBuffer()],
      program.programId
    );

    await program.methods
      .joinRound(currentRoundId)
      .accountsStrict({
        trader: admin.publicKey,
        config: configPda,
        round: roundPda,
        position: positionPda,
        leaderboard: leaderboardPda,
        traderTokenAccount: adminUsdcAta,
        vault: vaultPda,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();

    const round = await program.account.roundState.fetch(roundPda);
    expect(round.totalTraders).to.equal(1);
    expect(round.totalPool.toString()).to.equal(entryFee.toString());
  });

  it("3. Trader executes Long trade with 5x leverage and Pyth validation", async () => {
    const [roundPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("round"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [positionPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("position"), currentRoundId.toArrayLike(Buffer, "le", 8), admin.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .executeTrade(currentRoundId, 1, 5)
      .accountsStrict({
        trader: admin.publicKey,
        round: roundPda,
        position: positionPda,
        pythOracleAccount: mockPythPriceFeed.publicKey,
      })
      .rpc();

    const pos = await program.account.userPosition.fetch(positionPda);
    expect(pos.leverage).to.equal(5);
    expect(pos.tradeDirection).to.equal(1);
    expect(pos.entryPrice.toString()).to.equal("150000000");
  });

  it("4. Prevents duplicate position openings in identical round", async () => {
    const [roundPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("round"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [positionPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("position"), currentRoundId.toArrayLike(Buffer, "le", 8), admin.publicKey.toBuffer()],
      program.programId
    );

    try {
      await program.methods
        .executeTrade(currentRoundId, 2, 2)
        .accountsStrict({
          trader: admin.publicKey,
          round: roundPda,
          position: positionPda,
          pythOracleAccount: mockPythPriceFeed.publicKey,
        })
        .rpc();
      expect.fail("Expected failure on duplicate position execution.");
    } catch (err: any) {
      expect(err.error.errorCode.code).to.equal("PositionAlreadyOpen");
    }
  });

  it("5. Settle round and distribute prizes upon expiration", async () => {
    // Wait for end slot
    const [roundPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("round"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const round = await program.account.roundState.fetch(roundPda);
    let currentSlot = await provider.connection.getSlot();
    while (currentSlot <= round.endSlot.toNumber()) {
      await new Promise((r) => setTimeout(r, 400));
      currentSlot = await provider.connection.getSlot();
    }

    const [positionPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("position"), currentRoundId.toArrayLike(Buffer, "le", 8), admin.publicKey.toBuffer()],
      program.programId
    );
    const [leaderboardPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("leaderboard"), currentRoundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .settleRound(currentRoundId)
      .accountsStrict({
        keeper: admin.publicKey,
        round: roundPda,
        position: positionPda,
        leaderboard: leaderboardPda,
        pythOracleAccount: mockPythPriceFeed.publicKey,
      })
      .rpc();

    const roundAfter = await program.account.roundState.fetch(roundPda);
    expect(roundAfter.status).to.deep.equal({ settling: {} });

    // Claim prize
    const [configPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );
    const [vaultPda] = anchor.web3.Pubkey.findProgramAddressSync(
      [Buffer.from("vault"), usdcMint.toBuffer()],
      program.programId
    );

    const balanceBefore = (await provider.connection.getTokenAccountBalance(adminUsdcAta)).value.amount;

    await program.methods
      .claimPrize(currentRoundId)
      .accountsStrict({
        winner: admin.publicKey,
        config: configPda,
        round: roundPda,
        leaderboard: leaderboardPda,
        vault: vaultPda,
        winnerTokenAccount: adminUsdcAta,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();

    const balanceAfter = (await provider.connection.getTokenAccountBalance(adminUsdcAta)).value.amount;
    expect(BigInt(balanceAfter)).to.be.greaterThan(BigInt(balanceBefore));
  });
});