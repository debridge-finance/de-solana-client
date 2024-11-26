# de-solana-client

A wrapper around `solana-client` that enhances the base functionality with robust transaction handling, improved
querying capabilities, and comprehensive signature management.

## Additional Functionality Over Base Solana Client

This wrapper extends the standard Solana client with:

1. **Enhanced Transaction Handling**
    - Automatic transaction retry logic with customizable retry counts
    - Built-in transaction confirmation tracking
    - Configurable blockhash validation
    - Customizable confirmation timeouts and polling intervals
    - Error count threshold management

2. **Improved Account Querying**
    - Simplified program account querying with memory comparison filters
    - Type-safe memory offset and byte pattern matching
    - Streamlined account data retrieval

3. **Advanced Signature Management**
    - Comprehensive signature history retrieval
    - Automatic pagination handling for signature queries
    - Built-in duplicate signature detection
    - Sorted signature sets with slot and block time tracking

## Usage

### Transaction Sending with Retries

```rust
use de_solana_client::{RpcClient, AsyncResendTransactionWithSimpleStatus, SendContext};

#[tokio::main]
async fn main() {
    let client = RpcClient::new("https://api.mainnet-beta.solana.com".to_owned());

    // Define transaction builder
    let transaction_builder = |blockhash| {
        // Your transaction building logic here
        Transaction::new_with_payer(/* ... */)
    };

    // Send with retry logic
    let (signature, status) = client
        .resend_transaction_with_simple_status(
            transaction_builder,
            SendContext::default(),
            3  // Number of retries
        )
        .await
        .unwrap();
}
```

### Configurable Transaction Handling

```rust
use std::time::Duration;

// Custom confirmation settings
let context = SendContext {
confirm_duration: Duration::from_secs(60),    // Total time to wait for confirmation
confirm_request_pause: Duration::from_secs(1), // Time between status checks
blockhash_validation: true,                   // Validate blockhash before sending
ignorable_errors_count: 2,                    // Number of errors to ignore before failing
};
```

### Enhanced Signature History Retrieval

```rust
use de_solana_client::GetTransactionsSignaturesForAddress;

async fn get_signature_history(client: &RpcClient, address: &Pubkey) {
    // Automatically handles pagination and deduplication
    let signatures = client
        .get_signatures_data_for_address_with_config(
            address,
            CommitmentConfig::finalized(),
            None,  // Optional: until specific signature
        )
        .await
        .unwrap();
}
```

### Memory-Based Account Filtering

```rust
use de_solana_client::{GetProgramAccountsWithBytes, Memory};

async fn query_filtered_accounts(client: &RpcClient, program_id: Pubkey) {
    let accounts = client
        .get_program_accounts_with_bytes(
            &program_id,
            vec![
                Memory {
                    offset: 0,
                    bytes: vec![1, 2, 3], // Your filter pattern
                }
            ]
        )
        .await
        .unwrap();
}
```

## When to Use This Wrapper

Consider using this wrapper when you need:

- Robust transaction handling with automatic retries
- Simplified program account querying with memory filters
- Comprehensive signature history with automatic pagination
- Better error handling and logging integration
- Configurable transaction confirmation strategies