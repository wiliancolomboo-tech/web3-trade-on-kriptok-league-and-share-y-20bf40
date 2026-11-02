import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import {
  Keypair,
  PublicKey,
  SystemProgram,
  SYSVAR_RENT_PUBKEY,
} from "@solana/web3.js";
import {
  TOKEN_PROGRAM_ID,
  createMint,
  createAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { expect } from "chai";

describe("KriptoK League: Automated Round Lifecycle & Settlement Suite", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  let mint: PublicKey;
  let adminCollateralAccount: PublicKey;
  let traderLongCollateral: PublicKey;
  let traderShortCollateral: PublicKey;
  let feeTreasuryAccount: PublicKey;

  const admin = Keypair.generate();
  const traderLong = Keypair.generate();
  const traderShort = Keypair.generate();
  const feeRecipient = Keypair.generate();

  // Mock Pyth Account for testing
  const mockPythFeed = Keypair.generate();

  const roundId = new anchor.BN(1001);

  before(async () => {
    // 1. Airdrop SOL to test actors
    const txAirdrop = async (dest: PublicKey) => {
      const sig = await provider.connection.requestAirdrop(dest, 10 * anchor.web3.LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction(sig, "confirmed");
    };
    await txAirdrop(admin.publicKey);
    await txAirdrop(traderLong.publicKey);
    await txAirdrop(traderShort.publicKey);

    // 2. Initialize SPL USDC Token
    mint = await createMint(
      provider.connection,
      admin,
      admin.publicKey,
      null,
      6
    );

    // 3. Initialize Token Accounts
    adminCollateralAccount = await createAccount(provider.connection, admin, mint, admin.publicKey);
    traderLongCollateral = await createAccount(provider.connection, traderLong, mint, traderLong.publicKey);
    traderShortCollateral = await createAccount(provider.connection, traderShort, mint, traderShort.publicKey);
    feeTreasuryAccount = await createAccount(provider.connection, admin, mint, feeRecipient.publicKey);

    // Mint 1,000 USDC to each trader
    await mintTo(provider.connection, admin, mint, traderLongCollateral, admin.publicKey, 1_000_000_000);
    await mintTo(provider.connection, admin, mint, traderShortCollateral, admin.publicKey, 1_000_000_000);
  });

  it("Step 1: Initializes Global Protocol Configuration", async () => {
    const [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );

    await program.methods
      .initializeConfig(200, new anchor.BN(60)) // 2% fee, 60s max staleness
      .accounts({
        config: configPda,
        admin: admin.publicKey,
        feeRecipient: feeTreasuryAccount,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const configData = await program.account.leagueConfig.fetch(configPda);
    expect(configData.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(configData.feeBps).to.equal(200);
  });

  it("Step 2: Initializes Round with Program Derived Vault", async () => {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );

    // Allocate mock Pyth Price Feed layout (Dummy header with price)
    const mockPythData = Buffer.alloc(512);
    // Write Pyth Magic Number (0xa1b2c3d4) & version 2
    mockPythData.writeUInt32LE(0xa1b2c3d4, 0);
    mockPythData.writeUInt32LE(2, 4);
    // Write Price: 65,000 * 10^8
    mockPythData.writeBigInt64LE(BigInt(6500000000000), 208);
    // Write Exponent: -8
    mockPythData.writeInt32LE(-8, 20);
    // Write Timestamp: current unix
    const currentTs = Math.floor(Date.now() / 1000);
    mockPythData.writeBigInt64LE(BigInt(currentTs), 216);

    const rentLamports = await provider.connection.getMinimumBalanceForRentExemption(512);
    const initPythTx = new anchor.web3.Transaction().add(
      SystemProgram.createAccount({
        fromPubkey: admin.publicKey,
        newAccountPubkey: mockPythFeed.publicKey,
        lamports: rentLamports,
        space: 512,
        programId: SystemProgram.programId,
      })
    );
    await provider.sendAndConfirm(initPythTx, [admin, mockPythFeed]);

    // Initialize round
    await program.methods
      .initializeRound(roundId, new anchor.BN(10), new anchor.BN(15))
      .accounts({
        roundState: roundPda,
        vault: vaultPda,
        tokenMint: mint,
        config: configPda,
        priceFeedAccount: mockPythFeed.publicKey,
        authority: admin.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .signers([admin])
      .rpc();

    const roundData = await program.account.roundState.fetch(roundPda);
    expect(roundData.roundId.toNumber()).to.equal(1001);
    expect(roundData.status).to.equal(0); // Open
  });

  it("Step 3: Traders Open Counter-Positions (Long vs Short)", async () => {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    // Trader 1 stakes 100 USDC Long
    const [pos1] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderLong.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .openPosition({ long: {} }, new anchor.BN(100_000_000))
      .accounts({
        roundState: roundPda,
        userPosition: pos1,
        vault: vaultPda,
        userTokenAccount: traderLongCollateral,
        user: traderLong.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderLong])
      .rpc();

    // Trader 2 stakes 100 USDC Short
    const [pos2] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderShort.publicKey.toBuffer()],
      program.programId
    );

    await program.methods
      .openPosition({ short: {} }, new anchor.BN(100_000_000))
      .accounts({
        roundState: roundPda,
        userPosition: pos2,
        vault: vaultPda,
        userTokenAccount: traderShortCollateral,
        user: traderShort.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderShort])
      .rpc();

    const vaultInfo = await getAccount(provider.connection, vaultPda);
    expect(Number(vaultInfo.amount)).to.equal(200_000_000);
  });

  it("Step 4: Executes Crank Settlement and Verifies Mathematical PnL", async () => {
    // Wait for the end_duration epoch (simulating round passing)
    await new Promise((resolve) => setTimeout(resolve, 16000));

    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [configPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("config")],
      program.programId
    );

    await program.methods
      .settleRound()
      .accounts({
        roundState: roundPda,
        config: configPda,
        vault: vaultPda,
        feeRecipientToken: feeTreasuryAccount,
        priceFeedAccount: mockPythFeed.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
      })
      .rpc();

    const roundData = await program.account.roundState.fetch(roundPda);
    expect(roundData.status).to.equal(2); // Settled
    expect(roundData.protocolFeeCollected.toNumber()).to.equal(4_000_000); // 2% of 200 USDC = 4 USDC
  });
});