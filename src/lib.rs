use std::{
    fmt::Debug,
    future::Future,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base58::ToBase58;
pub use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::{
    client_error::ClientError,
    rpc_filter::{Memcmp, RpcFilterType},
    rpc_request::RpcError,
};
use solana_sdk::hash::Hash;
pub use solana_sdk::{
    self,
    account::Account,
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{Transaction, TransactionError},
};
use tokio::time;
use tracing::Level;

#[derive(Debug, Clone)]
pub struct SendContext {
    pub confirm_duration: Duration,
    pub confirm_request_pause: Duration,
    pub blockhash_validation: bool,
    pub ignorable_errors_count: usize,
}
impl Default for SendContext {
    fn default() -> Self {
        Self {
            confirm_duration: Duration::from_secs(60),
            confirm_request_pause: Duration::from_secs(1),
            blockhash_validation: true,
            ignorable_errors_count: 0,
        }
    }
}

#[async_trait]
pub trait AsyncSendTransaction {
    async fn get_latest_blockhash(&self) -> Result<Hash, ClientError>;

    async fn send_transaction_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: Transaction,
        expectant: &Expecter,
        send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>;

    async fn resend_transaction_with_custom_expectant<TransactionBuilder, Expecter, Fut, TxStatus>(
        &self,
        transaction_builder: TransactionBuilder,
        expectant: &Expecter,
        send_ctx: SendContext,
        mut resend_count: usize,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TransactionBuilder: Send + Sync + Fn(Hash) -> Transaction,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
    {
        loop {
            let tx = transaction_builder(self.get_latest_blockhash().await?);

            match self
                .send_transaction_with_custom_expectant::<Expecter, Fut, TxStatus>(
                    tx,
                    expectant,
                    send_ctx.clone(),
                )
                .await
            {
                Ok(result) => break Ok(result),
                Err(err) if resend_count != 0 => {
                    resend_count -= 1;
                    tracing::warn!(
                        "Error while send transaction: {:?}. Start resend. Resends left: {}",
                        err,
                        resend_count
                    );
                    continue;
                }
                Err(err) => break Err(err),
            }
        }
    }
}

#[async_trait]
impl AsyncSendTransaction for RpcClient {
    async fn get_latest_blockhash(&self) -> Result<Hash, ClientError> {
        self.get_latest_blockhash().await
    }

    async fn send_transaction_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: Transaction,
        expectant: &Expecter,
        mut send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
    {
        let span = tracing::span!(
            Level::TRACE,
            "send ",
            tx = format!("{:?}", transaction.signatures.first()).as_str()
        );
        let _guard = span.enter();
        if send_ctx.blockhash_validation {
            tracing::trace!(
                "Blockhash {} validation of transaction {:?} started",
                transaction.message.recent_blockhash,
                transaction
            );
            match self
                .is_blockhash_valid(
                    &transaction.message.recent_blockhash,
                    CommitmentConfig::processed(),
                )
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return Err(RpcError::ForUser(format!(
                        "Transaction {:?} blockhash not found by rpc",
                        transaction
                    ))
                    .into())
                }
                Err(err) => {
                    tracing::error!(
                        "Ignore error via blockhash request of {:?} transaction: {:?}. Error ignores left: {}",
                        transaction,
                        err,
                        send_ctx.ignorable_errors_count
                    );
                    return Err(RpcError::ForUser(format!(
                        "Error via transaction {:?} blockhash requesting",
                        transaction
                    ))
                    .into());
                }
            }
        }
        let signature = self.send_transaction(&transaction).await?;

        let instant = Instant::now();
        loop {
            match expectant(signature).await {
                Ok(None) => {
                    tracing::trace!(
                        "No status via sending {} transaction. Continue waiting",
                        signature
                    );
                }
                Ok(Some(status)) => {
                    tracing::trace!(
                        "Status of {} transaction, received: {:?}",
                        signature,
                        status
                    );
                    break Ok((signature, status));
                }
                Err(err) if send_ctx.ignorable_errors_count == 0 => {
                    tracing::error!(
                        "Error via status request of {} transaction: {:?}",
                        signature,
                        err,
                    );
                    break Err(err);
                }
                Err(err) => {
                    send_ctx.ignorable_errors_count -= 1;
                    tracing::error!(
                        "Ignore error via status request of {} transaction: {:?}. Error ignores left: {}",
                        signature,
                        err,
                        send_ctx.ignorable_errors_count
                    );
                }
            }

            if send_ctx.confirm_duration < instant.elapsed() {
                break Err(RpcError::ForUser(format!(
                    "Unable to confirm transaction {}.",
                    signature
                ))
                .into());
            }
            time::sleep(send_ctx.confirm_request_pause).await;
        }
    }
}

#[async_trait]
pub trait AsyncSendTransactionWithSimpleStatus: AsyncSendTransaction {
    async fn send_transaction_with_simple_status(
        &self,
        transaction: Transaction,
        send_ctx: SendContext,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>;
}

#[async_trait]
impl AsyncSendTransactionWithSimpleStatus for RpcClient {
    async fn send_transaction_with_simple_status(
        &self,
        transaction: Transaction,
        send_ctx: SendContext,
    ) -> Result<(Signature, Option<TransactionError>), ClientError> {
        self.send_transaction_with_custom_expectant(
            transaction,
            &|signature: Signature| async move {
                self.get_signature_status(&signature.clone()).await
            },
            send_ctx,
        )
        .await
        .map(|(signature, result_with_status)| (signature, result_with_status.err()))
    }
}
#[async_trait]
pub trait AsyncResendTransactionWithSimpleStatus: AsyncSendTransaction {
    async fn resend_transaction_with_simple_status<TransactionBuilder>(
        &self,
        transaction_builder: TransactionBuilder,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>
    where
        TransactionBuilder: Send + Sync + Fn(Hash) -> Transaction;
}

#[async_trait]
impl AsyncResendTransactionWithSimpleStatus for RpcClient {
    async fn resend_transaction_with_simple_status<TransactionBuilder>(
        &self,
        transaction_builder: TransactionBuilder,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>
    where
        TransactionBuilder: Send + Sync + Fn(Hash) -> Transaction,
    {
        self.resend_transaction_with_custom_expectant(
            transaction_builder,
            &|signature: Signature| async move {
                self.get_signature_status(&signature.clone()).await
            },
            send_ctx,
            resend_count,
        )
        .await
        .map(|(signature, result_with_status)| (signature, result_with_status.err()))
    }
}

pub struct Memory {
    pub offset: usize,
    pub bytes: Vec<u8>,
}
impl From<Memory> for RpcFilterType {
    fn from(mem: Memory) -> RpcFilterType {
        RpcFilterType::Memcmp(Memcmp {
            offset: mem.offset,
            bytes: solana_client::rpc_filter::MemcmpEncodedBytes::Base58(mem.bytes.to_base58()),
            encoding: None,
        })
    }
}

#[async_trait]
pub trait GetProgramAccountsWithBytes {
    async fn get_program_accounts_with_bytes(
        &self,
        program: &Pubkey,
        bytes: Vec<Memory>,
    ) -> Result<Vec<(Pubkey, Account)>, ClientError>;
}

use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};

#[async_trait]
impl GetProgramAccountsWithBytes for RpcClient {
    async fn get_program_accounts_with_bytes(
        &self,
        program_id: &Pubkey,
        bytes: Vec<Memory>,
    ) -> Result<Vec<(Pubkey, Account)>, ClientError> {
        use solana_account_decoder::*;
        Ok(self
            .get_program_accounts_with_config(
                program_id,
                RpcProgramAccountsConfig {
                    filters: Some(bytes.into_iter().map(RpcFilterType::from).collect()),
                    account_config: RpcAccountInfoConfig {
                        encoding: Some(UiAccountEncoding::Base64),
                        ..Default::default()
                    },
                    with_context: None,
                },
            )
            .await?)
    }
}
