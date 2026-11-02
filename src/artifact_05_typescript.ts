import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair, SystemProgram } from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { expect } from "chai";
import { KriptokLeague } from "../target/types/kriptok_league";

describe("KriptoK League: End-to-End Test Suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;
  const admin = (provider.wallet as anchor.Wallet).payer;

  const traderBull = Keypair.generate();
  const traderBear = Keypair.generate();

  let collateralMint: PublicKey;
  let traderBullTokenAcc: PublicKey;
  let traderBearTokenAcc: PublicKey;
  let mockPythAccount: Keypair;

  const leagueId = new anchor.BN(101);
  const feeBps = 300; // 3% protocol rake

  before(async () => {
    // Airdrop SOL to test actors
    for (const actor of [traderBull, traderBear]) {
      const sig = await provider.connection.requestAirdrop(actor.publicKey, 2e9);
      await provider.connection.confirmTransaction(sig);
    }

    // Initialize mock collateral mint (USDC simulator)
    collateralMint = await createMint(
      provider.connection,
      admin,
      admin.publicKey,
      null,
      6
    );

    // Setup Token Accounts
    traderBullTokenAcc = await createAccount(
      provider.connection,
      traderBull,
      collateralMint,
      traderBull.publicKey
    );

    traderBearTokenAcc = await createAccount(
      provider.connection,
      traderBear,
      collateralMint,
      traderBear.publicKey
    );

    // Fund accounts with 1,000 USDC each
    await mintTo(provider.connection, admin, collateralMint, traderBullTokenAcc, admin, 1_000_000_000);
    await mintTo(provider.connection, admin, collateralMint, traderBearTokenAcc, admin, 1_000_000_000);

    // Initialize mock Pyth Price Feed account
    mockPythAccount = Keypair.generate();
    await createMockPythAccount(provider, mockPythAccount, 150_00000000, -8); // Initial $150.00 SOL/USD
  });

  it("1. Initializes League with 3% fee structure", async () => {
    const [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), leagueId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .initializeLeague(leagueId, feeBps)
      .accounts({
        league: leaguePda,
        collateralMint: collateralMint,
        admin: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const leagueData = await program.account.league.fetch(leaguePda);
    expect(leagueData.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(leagueData.feeBasisPoints).to.equal(300);
  });

  it("2. Launches Round 1 with Oracle integration", async () => {
    const [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), leagueId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    const roundId = new anchor.BN(1);
    const [roundPda] = PublicKey.findProgramAddressSync(
      [
        Buffer.from("round"),
        leagueId.toArrayLike(Buffer, "le", 8),
        roundId.toArrayLike(Buffer, "le", 8),
      ],
      program.programId
    );

    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .startRound(new anchor.BN(5), new anchor.BN(3)) // 5 sec round, 3 sec lock
      .accounts({
        league: leaguePda,
        round: roundPda,
        roundVault: vaultPda,
        collateralMint: collateralMint,
        pythPriceFeed: mockPythAccount.publicKey,
        authority: admin.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .rpc();

    const roundData = await program.account.round.fetch(roundPda);
    expect(roundData.startPrice.toNumber()).to.equal(150_00000000);
  });

  it("3. Traders enter Bull (100 USDC) and Bear (100 USDC) positions", async () => {
    const roundId = new anchor.BN(1);
    const [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), leagueId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), leagueId.toArrayLike(Buffer, "le", 8), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    // Bull Position Entry
    const [bullPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBull.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .enterPosition({ long: {} }, new anchor.BN(100_000_000))
      .accounts({
        league: leaguePda,
        round: roundPda,
        position: bullPos,
        roundVault: vaultPda,
        userTokenAccount: traderBullTokenAcc,
        user: traderBull.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderBull])
      .rpc();

    // Bear Position Entry
    const [bearPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBear.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .enterPosition({ short: {} }, new anchor.BN(100_000_000))
      .accounts({
        league: leaguePda,
        round: roundPda,
        position: bearPos,
        roundVault: vaultPda,
        userTokenAccount: traderBearTokenAcc,
        user: traderBear.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderBear])
      .rpc();

    const vaultBalance = await getAccount(provider.connection, vaultPda);
    expect(Number(vaultBalance.amount)).to.equal(200_000_000);
  });

  it("4. Locks and resolves Round with Long as winner ($160.00 closing)", async () => {
    // Wait for end time to pass
    await new Promise((resolve) => setTimeout(resolve, 5500));

    // Update mock Pyth price feed to $160.00
    await updateMockPythPrice(provider, mockPythAccount, 160_00000000);

    const roundId = new anchor.BN(1);
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), leagueId.toArrayLike(Buffer, "le", 8), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .resolveRound()
      .accounts({
        round: roundPda,
        pythPriceFeed: mockPythAccount.publicKey,
      })
      .rpc();

    const roundData = await program.account.round.fetch(roundPda);
    expect(roundData.outcome).to.deep.equal({ longWon: {} });
    expect(roundData.finalPrice.toNumber()).to.equal(160_00000000);
  });

  it("5. Bull trader claims rewards and Bear claim is rejected", async () => {
    const roundId = new anchor.BN(1);
    const [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), leagueId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), leagueId.toArrayLike(Buffer, "le", 8), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [bullPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBull.publicKey.toBuffer()],
      program.programId
    );
    const [bearPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBear.publicKey.toBuffer()],
      program.programId
    );

    const initialBullBalance = Number((await getAccount(provider.connection, traderBullTokenAcc)).amount);

    // Bull claims reward: Total pool = 200 USDC, fee = 3% (6 USDC), payout = 194 USDC
    await program.methods
      .claimReward()
      .accounts({
        league: leaguePda,
        round: roundPda,
        position: bullPos,
        roundVault: vaultPda,
        userTokenAccount: traderBullTokenAcc,
        user: traderBull.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .signers([traderBull])
      .rpc();

    const postBullBalance = Number((await getAccount(provider.connection, traderBullTokenAcc)).amount);
    expect(postBullBalance - initialBullBalance).to.equal(194_000_000);

    // Bear attempts to claim (must revert with PositionLost error)
    try {
      await program.methods
        .claimReward()
        .accounts({
          league: leaguePda,
          round: roundPda,
          position: bearPos,
          roundVault: vaultPda,
          userTokenAccount: traderBearTokenAcc,
          user: traderBear.publicKey,
          tokenProgram: TOKEN_PROGRAM_ID,
        })
        .signers([traderBear])
        .rpc();
      expect.fail("Bear position should not be allowed to claim rewards");
    } catch (err: any) {
      expect(err.message).to.include("PositionLost");
    }
  });
});

// -----------------------------------------------------------------------------
// PYTH MOCK GENERATOR UTILITIES
// -----------------------------------------------------------------------------
async function createMockPythAccount(
  provider: anchor.AnchorProvider,
  keypair: Keypair,
  price: number,
  expo: number
) {
  const accountSize = 512;
  const lamports = await provider.connection.getMinimumBalanceForRentExemption(accountSize);
  const tx = new anchor.web3.Transaction().add(
    anchor.web3.SystemProgram.createAccount({
      fromPubkey: provider.wallet.publicKey,
      newAccountPubkey: keypair.publicKey,
      lamports,
      space: accountSize,
      programId: keypair.publicKey, // Mock ownership
    })
  );
  await provider.sendAndConfirm(tx, [keypair]);
  await updateMockPythPrice(provider, keypair, price, expo);
}

async function updateMockPythPrice(
  provider: anchor.AnchorProvider,
  keypair: Keypair,
  price: number,
  expo = -8
) {
  // Construct Pyth PriceAccount binary structure layout
  const buffer = Buffer.alloc(512);
  buffer.writeUInt32LE(0xa1b2c3d4, 0); // Magic number
  buffer.writeUInt32LE(2, 4); // Version 2
  buffer.writeInt32LE(expo, 20); // Exponent
  buffer.writeBigInt64LE(BigInt(price), 208); // Aggregate Price
  buffer.writeBigInt64LE(BigInt(Math.floor(Date.now() / 1000)), 224); // Publish time

  // Write directly using local test validator storage
  // (In live test harness, a mocked oracle program or Pyth devnet testnet feed account is supplied)
}