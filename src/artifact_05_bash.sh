# 1. Clone repository and navigate to root directory
cd kriptok-league

# 2. Synchronize program ID
anchor keys sync

# 3. Compile the Anchor program to optimize binary size
cargo build-sbf --manifest-path programs/kriptok-league/Cargo.toml