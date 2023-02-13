use std::str::FromStr;

use de_solana_client::{solana_sdk::signature, *};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client = RpcClient::new("https://api.mainnet-beta.solana.com".to_owned());
    let _payer =
        signature::read_keypair_file(dirs::config_dir().unwrap().join("solana").join("id.json"))
            .unwrap();

    let signatures = client
        .get_signatures_data_for_address_with_config(
            &Pubkey::from_str("2keGwZ2ktgK7oBCPbg7hbdSmBUjo23rEtqsWdqh57Rsf").unwrap(),
            CommitmentConfig::finalized(),
            None,
        )
        .await
        .unwrap();

    println!("{signatures:?}");
    println!("{}", signatures.len());
}
