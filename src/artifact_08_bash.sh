# 1. Clone the repository and navigate to workspace
git clone https://github.com/kriptok-sentinel/kriptok-league-engine.git
cd kriptok-league-engine

# 2. Install TypeScript dependencies
npm install

# 3. Synchronize program keys
anchor keys sync

# 4. Compile Anchor Rust program to BPF bytecode
anchor build

# 5. Execute full end-to-end integration and security test suite
anchor test