import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { KriptoKLeagueEngine } from "../target/types/kriptok_league_engine";
import {
  createMint,
  createAccount,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { expect } from "chai";

describe("kriptok_league_engine tests", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.KriptoKLeagueEngine as Program<KriptoKLeagueEngine>;
  let vaultMint: PublicKey;
  let userTokenAccount: PublicKey;
  let leaguePda: PublicKey;
  const payer = (provider.wallet as anchor.Wallet).payer;

  before(async () => {
    // 1. Create Mock USDC Mint
    vaultMint = await createMint(
      provider.connection,
      payer,
      payer.publicKey,
      null,
      6
    );

    // 2. Setup user token account and mint 1,000 USDC
    userTokenAccount = await createAccount(
      provider.connection,
      payer,
      vaultMint,
      payer.publicKey
    );

    await mintTo(
      provider.connection,
      payer,
      vaultMint,
      userTokenAccount,
      payer,
      1_000_000_000
    );

    [leaguePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("league"), vaultMint.toBuffer()],
      program.programId
    );
  });

  it("Initializes the KriptoK League", async () => {
    await program.methods
      .initializeLeague(new anchor.BN(60)) // 60-second round duration
      .accounts({
        league: leaguePda,
        vaultMint: vaultMint,
        authority: payer.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const league = await program.account.league.fetch(leaguePda);
    expect(league.authority.toString()).to.equal(payer.publicKey.toString());
    expect(league.roundDurationSecs.toNumber()).to.equal(60);
  });
});