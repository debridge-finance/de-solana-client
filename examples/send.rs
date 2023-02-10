use std::{
    str::FromStr,
    time::{Duration, SystemTime},
};

use de_solana_client::{
    solana_sdk::signature::{self, Signer},
    *,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client = RpcClient::new("https://api.mainnet-beta.solana.com".to_owned());
    let payer =
        signature::read_keypair_file(dirs::config_dir().unwrap().join("solana").join("id.json"))
            .unwrap();

    let mut signatures = client
        .get_signatures_data_for_address_with_config(
            &Pubkey::from_str("2keGwZ2ktgK7oBCPbg7hbdSmBUjo23rEtqsWdqh57Rsf").unwrap(),
            CommitmentConfig::finalized(),
            None,
        )
        .await
        .unwrap();
    signatures.reverse();

    println!("{signatures:?}");
    println!("{}", signatures.len());
}
