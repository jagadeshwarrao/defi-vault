import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { DefiVault } from "../target/types/defi_vault";
import {
  createMint,
  createAssociatedTokenAccount,
  mintTo,
  getAccount,
} from "@solana/spl-token";
import { assert } from "chai";

describe("defi-vault", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.DefiVault as Program<DefiVault>;
  const admin = provider.wallet as anchor.Wallet;

  let depositMint: anchor.web3.PublicKey;
  let rewardMint: anchor.web3.PublicKey;
  let vaultState: anchor.web3.PublicKey;
  let vaultTreasury: anchor.web3.PublicKey;
  let rewardTreasury: anchor.web3.PublicKey;
  let userTokenAccount: anchor.web3.PublicKey;
  let userRewardAccount: anchor.web3.PublicKey;
  let userPosition: anchor.web3.PublicKey;

  const REWARD_RATE = new anchor.BN(1_000); // 1000 units/sec per token

  before(async () => {
    // Create mints
    depositMint = await createMint(provider.connection, admin.payer, admin.publicKey, null, 6);
    rewardMint = await createMint(provider.connection, admin.payer, admin.publicKey, null, 6);

    // Derive PDAs
    [vaultState] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("vault_state"), depositMint.toBuffer()],
      program.programId
    );
    [vaultTreasury] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("vault_treasury"), depositMint.toBuffer()],
      program.programId
    );
    [rewardTreasury] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("reward_treasury"), rewardMint.toBuffer()],
      program.programId
    );
    [userPosition] = anchor.web3.PublicKey.findProgramAddressSync(
      [Buffer.from("user_position"), vaultState.toBuffer(), admin.publicKey.toBuffer()],
      program.programId
    );

    // User token accounts
    userTokenAccount = await createAssociatedTokenAccount(
      provider.connection, admin.payer, depositMint, admin.publicKey
    );
    userRewardAccount = await createAssociatedTokenAccount(
      provider.connection, admin.payer, rewardMint, admin.publicKey
    );

    // Mint deposit tokens to user
    await mintTo(provider.connection, admin.payer, depositMint, userTokenAccount, admin.payer, 1_000_000);
    // Mint reward tokens to reward treasury (pre-fund)
    // (treasury is created in initialize, fund after)
  });

  it("initializes the vault", async () => {
    await program.methods
      .initialize(REWARD_RATE)
      .accounts({
        vaultState,
        vaultTreasury,
        rewardTreasury,
        depositMint,
        rewardMint,
        admin: admin.publicKey,
      })
      .rpc();

    const state = await program.account.vaultState.fetch(vaultState);
    assert.equal(state.admin.toBase58(), admin.publicKey.toBase58());
    assert.equal(state.rewardRatePerSecond.toNumber(), REWARD_RATE.toNumber());
    assert.equal(state.totalStaked.toNumber(), 0);

    // Fund reward treasury after initialization
    await mintTo(provider.connection, admin.payer, rewardMint, rewardTreasury, admin.payer, 10_000_000);
  });

  it("deposits tokens", async () => {
    const depositAmount = new anchor.BN(100_000);

    await program.methods
      .deposit(depositAmount)
      .accounts({
        vaultState,
        userPosition,
        vaultTreasury,
        userTokenAccount,
        user: admin.publicKey,
      })
      .rpc();

    const state = await program.account.vaultState.fetch(vaultState);
    const position = await program.account.userPosition.fetch(userPosition);

    assert.equal(state.totalStaked.toNumber(), 100_000);
    assert.equal(position.stakedAmount.toNumber(), 100_000);

    const treasury = await getAccount(provider.connection, vaultTreasury);
    assert.equal(Number(treasury.amount), 100_000);
  });

  it("accrues rewards over time", async () => {
    // Advance ~2 slots (simulated time); in real tests use bankrun for time travel
    await new Promise((r) => setTimeout(r, 2000));

    const position = await program.account.userPosition.fetch(userPosition);
    const state = await program.account.vaultState.fetch(vaultState);

    // reward_per_token_stored should have advanced
    assert.isAbove(state.rewardPerTokenStored.toNumber(), 0, "Accumulator should increase");
    console.log("reward_per_token_stored:", state.rewardPerTokenStored.toString());
  });

  it("claims rewards", async () => {
    await program.methods
      .claimRewards()
      .accounts({
        vaultState,
        userPosition,
        rewardTreasury,
        userRewardAccount,
        user: admin.publicKey,
      })
      .rpc();

    const rewardAcct = await getAccount(provider.connection, userRewardAccount);
    console.log("Rewards received:", rewardAcct.amount.toString());
    assert.isAbove(Number(rewardAcct.amount), 0, "Should have received rewards");
  });

  it("withdraws tokens", async () => {
    const withdrawAmount = new anchor.BN(50_000);

    await program.methods
      .withdraw(withdrawAmount)
      .accounts({
        vaultState,
        userPosition,
        vaultTreasury,
        userTokenAccount,
        user: admin.publicKey,
      })
      .rpc();

    const position = await program.account.userPosition.fetch(userPosition);
    assert.equal(position.stakedAmount.toNumber(), 50_000);
  });

  it("admin can update reward rate", async () => {
    const newRate = new anchor.BN(2_000);
    await program.methods
      .setRewardRate(newRate)
      .accounts({ vaultState, admin: admin.publicKey })
      .rpc();

    const state = await program.account.vaultState.fetch(vaultState);
    assert.equal(state.rewardRatePerSecond.toNumber(), 2_000);
  });

  it("admin can pause vault", async () => {
    await program.methods
      .setPaused(true)
      .accounts({ vaultState, admin: admin.publicKey })
      .rpc();

    const depositAmount = new anchor.BN(1_000);
    try {
      await program.methods
        .deposit(depositAmount)
        .accounts({ vaultState, userPosition, vaultTreasury, userTokenAccount, user: admin.publicKey })
        .rpc();
      assert.fail("Should have thrown VaultPaused error");
    } catch (err) {
      assert.include(err.toString(), "VaultPaused");
    }

    // Unpause
    await program.methods
      .setPaused(false)
      .accounts({ vaultState, admin: admin.publicKey })
      .rpc();
  });
});
