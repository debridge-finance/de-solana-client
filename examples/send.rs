use std::time::{Duration, SystemTime};

use de_solana_client::{
    solana_sdk::signature::{self, Signer},
    *,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let client = RpcClient::new("https://api.devnet.solana.com".to_owned());
    let payer =
        signature::read_keypair_file(dirs::config_dir().unwrap().join("solana").join("id.json"))
            .unwrap();
    let (signature, status) = client
        .send_transaction_with_simple_status(
            Transaction::new_signed_with_payer(
                &[spl_memo::build_memo(
                    format!("{:?}", SystemTime::now()).as_bytes(),
                    &[&payer.pubkey()],
                )],
                Some(&payer.pubkey()),
                &[&payer],
                client.get_latest_blockhash().await.unwrap(),
            ),
            SendContext {
                confirm_duration: Duration::from_secs(60 * 5),
                confirm_request_pause: Duration::from_secs(1),
                blockhash_validation: true,
                ignorable_errors_count: 0,
            },
        )
        .await
        .unwrap();

    println!(
        "Transaction with signature {signature} {status}",
        signature = signature,
        status = match status {
            Some(err) => format!("sent with error {:?}", err),
            None => "sent and confirmed".to_owned(),
        }
    );
}
