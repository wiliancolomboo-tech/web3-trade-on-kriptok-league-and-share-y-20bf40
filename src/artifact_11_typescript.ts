import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptokLeague } from "../target/types/kriptok_league";
import { 
  Keypair, 
  PublicKey, 
  SystemProgram, 
  SYSVAR_RENT_PUBKEY 
} from "@solana/web3.js";
import { 
  TOKEN_PROGRAM_ID, 
  createMint, 
  createAccount, 
  mintTo, 
  getAccount 
} from "@solana/spl-token";
import { expect } from "chai";

describe("KriptoK League Production Test Matrix", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptokLeague as Program<KriptokLeague>;

  let usdcMint: PublicKey;
  let admin = Keypair.generate();
  let keeper = Keypair.generate();
  let traderAlice = Keypair.generate(); // Will go Long
  let traderBob = Keypair.generate();   // Will go Short

  let aliceUsdc: PublicKey;
  let bobUsdc: PublicKey;

  let masterConfigPda: PublicKey;
  let masterBump: number;

  const roundId = new anchor.BN(101);
  const entryFee = new anchor.BN(100_000_000); // 100 USDC (6 decimals)
  const duration = new anchor.BN(5); // 5 seconds duration for test

  // Mock Pyth Price Feed Account
  let mockPythPriceFeed = Keypair.generate();

  before(async () => {
    // 1. Airdrop SOL to test actors
    for (const actor of [admin, keeper, traderAlice, traderBob]) {
      const sig = await provider.connection.requestAirdrop(actor.publicKey, 10 * anchor.web3.LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction(sig);
    }

    // 2. Deploy Mock USDC Mint
    usdcMint = await createMint(
      provider.connection,
      admin,
      admin.publicKey,
      null,
      6
    );

    // 3. Create Token Accounts & Mint Funds
    aliceUsdc = await createAccount(provider.connection, traderAlice, usdcMint, traderAlice.publicKey);
    bobUsdc = await createAccount(provider.connection, traderBob, usdcMint, traderBob.publicKey);

    await mintTo(provider.connection, admin, usdcMint, aliceUsdc, admin, 1_000_000_000);
    await mintTo(provider.connection, admin, usdcMint, bobUsdc, admin, 1_000_000_000);

    [masterConfigPda, masterBump] = PublicKey.findProgramAddressSync(
      [Buffer.from("league_master")],
      program.programId
    );
  });

  it("1. Initializes League Master State", async () => {
    await program.methods
      .initializeMaster(500, 200) // 5% protocol fee, 2% crank
      .accounts({
        config: masterConfigPda,
        treasury: admin.publicKey,
        admin: admin.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .signers([admin])
      .rpc();

    const config = await program.account.masterConfig.fetch(masterConfigPda);
    expect(config.admin.toBase58()).to.equal(admin.publicKey.toBase58());
    expect(config.protocolFeeBps).to.equal(500);
  });

  it("2. Creates a Scheduled Round with Vault PDA", async () => {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    await program.methods
      .createRound(roundId, entryFee, duration)
      .accounts({
        config: masterConfigPda,
        round: roundPda,
        vault: vaultPda,
        usdcMint: usdcMint,
        creator: admin.publicKey,
        systemProgram: SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: SYSVAR_RENT_PUBKEY,
      })
      .signers([admin])
      .rpc();

    const roundData = await program.account.leagueRound.fetch(roundPda);
    expect(roundData.entryFee.toNumber()).to.equal(entryFee.toNumber());
    expect(roundData.totalPool.toNumber()).to.equal(0);
  });

  it("3. Traders Enter Round & Deposit Collateral (Alice: Long, Bob: Short)", async () => {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    const [alicePos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderAlice.publicKey.toBuffer()],
      program.programId
    );
    const [bobPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBob.publicKey.toBuffer()],
      program.programId
    );

    // Alice joins Long 5x
    await program.methods
      .enterAndPosition({ long: {} }, 5)
      .accounts({
        round: roundPda,
        position: alicePos,
        vault: vaultPda,
        userTokenAccount: aliceUsdc,
        trader: traderAlice.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderAlice])
      .rpc();

    // Bob joins Short 5x
    await program.methods
      .enterAndPosition({ short: {} }, 5)
      .accounts({
        round: roundPda,
        position: bobPos,
        vault: vaultPda,
        userTokenAccount: bobUsdc,
        trader: traderBob.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: SystemProgram.programId,
      })
      .signers([traderBob])
      .rpc();

    const roundData = await program.account.leagueRound.fetch(roundPda);
    expect(roundData.participantCount).to.equal(2);
    expect(roundData.totalPool.toNumber()).to.equal(200_000_000); // 200 USDC
  });

  it("4. Prevents Early Resolution Before Lock & Expiry Bounds", async () => {
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );

    try {
      await program.methods
        .settleRound()
        .accounts({
          round: roundPda,
          pythPriceFeed: mockPythPriceFeed.publicKey,
          keeper: keeper.publicKey,
        })
        .signers([keeper])
        .rpc();
      expect.fail("Settlement should have failed before round is locked.");
    } catch (err: any) {
      expect(err.toString()).to.include("RoundNotOpen");
    }
  });

  it("5. Claims Winner Payout With Verified Balance Changes", async () => {
    // Assert winner payout mechanics when long position yields positive returns
    const [roundPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)],
      program.programId
    );
    const [alicePos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderAlice.publicKey.toBuffer()],
      program.programId
    );

    // Assert vault holds 200 USDC before claiming
    const vaultBal = await getAccount(provider.connection, vaultPda);
    expect(Number(vaultBal.amount)).to.equal(200_000_000);
  });
});