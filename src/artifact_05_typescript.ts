import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair } from "@solana/web3.js";
import {
  createMint,
  createAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { expect } from "chai";
import { KriptoKLeague } from "../target/types/kriptok_league";

describe("KriptoK League Core Protocol & Round Lifecycle", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptoKLeague as Program<KriptoKLeague>;

  let usdcMint: PublicKey;
  let treasuryAta: PublicKey;
  let traderBull: Keypair;
  let traderBullAta: PublicKey;
  let traderBear: Keypair;
  let traderBearAta: PublicKey;

  const roundId = new anchor.BN(101);
  let configPda: PublicKey;
  let roundPda: PublicKey;
  let vaultPda: PublicKey;

  before(async () => {
    traderBull = Keypair.generate();
    traderBear = Keypair.generate();

    // Fund SOL for gas
    await provider.connection.confirmTransaction(
      await provider.connection.requestAirdrop(traderBull.publicKey, 2 * anchor.web3.LAMPORTS_PER_SOL)
    );
    await provider.connection.confirmTransaction(
      await provider.connection.requestAirdrop(traderBear.publicKey, 2 * anchor.web3.LAMPORTS_PER_SOL)
    );

    // Create Mock USDC Mint (6 Decimals)
    usdcMint = await createMint(
      provider.connection,
      (provider.wallet as any).payer,
      provider.wallet.publicKey,
      null,
      6
    );

    // Initialize Treasury Account
    treasuryAta = await createAccount(
      provider.connection,
      (provider.wallet as any).payer,
      usdcMint,
      provider.wallet.publicKey
    );

    // Create Trader Token Accounts and mint balance
    traderBullAta = await createAccount(
      provider.connection,
      traderBull,
      usdcMint,
      traderBull.publicKey
    );
    traderBearAta = await createAccount(
      provider.connection,
      traderBear,
      usdcMint,
      traderBear.publicKey
    );

    await mintTo(provider.connection, (provider.wallet as any).payer, usdcMint, traderBullAta, provider.wallet.publicKey, 1_000 * 1_000_000);
    await mintTo(provider.connection, (provider.wallet as any).payer, usdcMint, traderBearAta, provider.wallet.publicKey, 1_000 * 1_000_000);

    [configPda] = PublicKey.findProgramAddressSync([Buffer.from("config")], program.programId);
    [roundPda] = PublicKey.findProgramAddressSync([Buffer.from("round"), roundId.toArrayLike(Buffer, "le", 8)], program.programId);
    [vaultPda] = PublicKey.findProgramAddressSync([Buffer.from("vault"), roundId.toArrayLike(Buffer, "le", 8)], program.programId);
  });

  it("1. Initializes global protocol configuration with 2.5% fee", async () => {
    const feeBps = 250; // 2.50%
    const minStake = new anchor.BN(1 * 1_000_000); // 1 USDC
    const maxStake = new anchor.BN(500 * 1_000_000); // 500 USDC

    await program.methods
      .initializeProtocol(feeBps, minStake, maxStake)
      .accounts({
        config: configPda,
        mint: usdcMint,
        treasury: treasuryAta,
        admin: provider.wallet.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .rpc();

    const config = await program.account.protocolConfig.fetch(configPda);
    expect(config.feeBasisPoints).to.equal(250);
    expect(config.admin.toBase58()).to.equal(provider.wallet.publicKey.toBase58());
  });

  it("2. Initializes Round #101 with sequential windows", async () => {
    const now = Math.floor(Date.now() / 1000);
    const startTime = new anchor.BN(now);
    const lockTime = new anchor.BN(now + 4);
    const settleTime = new anchor.BN(now + 8);

    await program.methods
      .initializeRound(roundId, startTime, lockTime, settleTime)
      .accounts({
        round: roundPda,
        roundVault: vaultPda,
        mint: usdcMint,
        creator: provider.wallet.publicKey,
        systemProgram: anchor.web3.SystemProgram.programId,
        tokenProgram: TOKEN_PROGRAM_ID,
        rent: anchor.web3.SYSVAR_RENT_PUBKEY,
      })
      .rpc();

    const round = await program.account.roundAccount.fetch(roundPda);
    expect(round.roundId.toNumber()).to.equal(101);
    expect(round.totalBullAmount.toNumber()).to.equal(0);
    expect(round.totalBearAmount.toNumber()).to.equal(0);
  });

  it("3. Accepts Bull and Bear positions into escrow vault", async () => {
    const [bullPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBull.publicKey.toBuffer()],
      program.programId
    );
    const [bearPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), traderBear.publicKey.toBuffer()],
      program.programId
    );

    const stakeAmount = new anchor.BN(100 * 1_000_000); // 100 USDC

    // Trader Bull enters
    await program.methods
      .enterPosition(roundId, { bull: {} }, stakeAmount)
      .accounts({
        round: roundPda,
        config: configPda,
        position: bullPos,
        roundVault: vaultPda,
        userTokenAccount: traderBullAta,
        trader: traderBull.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([traderBull])
      .rpc();

    // Trader Bear enters
    await program.methods
      .enterPosition(roundId, { bear: {} }, stakeAmount)
      .accounts({
        round: roundPda,
        config: configPda,
        position: bearPos,
        roundVault: vaultPda,
        userTokenAccount: traderBearAta,
        trader: traderBear.publicKey,
        tokenProgram: TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([traderBear])
      .rpc();

    const vaultTokenAccount = await getAccount(provider.connection, vaultPda);
    expect(Number(vaultTokenAccount.amount)).to.equal(200 * 1_000_000);
  });

  it("4. Rejects position entry when below minimum stake", async () => {
    const subMinStake = new anchor.BN(500_000); // 0.5 USDC
    const dummyUser = Keypair.generate();
    const [dummyPos] = PublicKey.findProgramAddressSync(
      [Buffer.from("position"), roundId.toArrayLike(Buffer, "le", 8), dummyUser.publicKey.toBuffer()],
      program.programId
    );

    try {
      await program.methods
        .enterPosition(roundId, { bull: {} }, subMinStake)
        .accounts({
          round: roundPda,
          config: configPda,
          position: dummyPos,
          roundVault: vaultPda,
          userTokenAccount: traderBullAta,
          trader: traderBull.publicKey,
          tokenProgram: TOKEN_PROGRAM_ID,
          systemProgram: anchor.web3.SystemProgram.programId,
        })
        .signers([traderBull])
        .rpc();
      expect.fail("Expected transaction to fail with StakeTooSmall");
    } catch (err: any) {
      expect(err.toString()).to.include("StakeTooSmall");
    }
  });
});