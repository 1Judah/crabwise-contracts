// Generated token client calls propagate contract invocation failures. Do not
// convert a failed transfer into success: Soroban rolls back the enclosing
// escrow invocation, including any earlier state writes or token transfers.
soroban_sdk::contractimport!(file = "../../target/wasm32-unknown-unknown/release/soroban_token_contract.wasm");
