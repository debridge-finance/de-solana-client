use std::{
    collections::BTreeSet,
    fmt::Debug,
    future::Future,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use base58::ToBase58;
use itertools::{FoldWhile, Itertools};
pub use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::{
    client_error::ClientError,
    rpc_client::{GetConfirmedSignaturesForAddress2Config, SerializableTransaction},
    rpc_filter::{Memcmp, RpcFilterType},
    rpc_request::RpcError,
};
pub use solana_sdk::{
    self,
    account::Account,
    commitment_config::CommitmentConfig,
    pubkey::Pubkey,
    signature::Signature,
    transaction::{Transaction, TransactionError},
};
use solana_sdk::{clock::UnixTimestamp, hash::Hash};
use tokio::time;
use tracing::{instrument, Level};

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

    async fn send_transaction_with_config_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        config: RpcSendTransactionConfig,
        expectant: &Expecter,
        send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>;

    async fn send_transaction_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        expectant: &Expecter,
        send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>;

    async fn resend_transaction_with_custom_expectant<
        TransactionBuilder,
        Expecter,
        Fut,
        TxStatus,
        Tx,
    >(
        &self,
        transaction_builder: TransactionBuilder,
        expectant: &Expecter,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TransactionBuilder: Send + Sync + Fn(Hash) -> Tx,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
        Tx: SerializableTransaction + Send + Sync,
    {
        self.resend_transaction_with_config_with_custom_expectant(
            transaction_builder,
            Default::default(),
            expectant,
            send_ctx,
            resend_count,
        )
        .await
    }

    async fn resend_transaction_with_config_with_custom_expectant<
        TransactionBuilder,
        Expecter,
        Fut,
        TxStatus,
        Tx,
    >(
        &self,
        transaction_builder: TransactionBuilder,
        config: RpcSendTransactionConfig,
        expectant: &Expecter,
        send_ctx: SendContext,
        mut resend_count: usize,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TransactionBuilder: Send + Sync + Fn(Hash) -> Tx,
        Tx: SerializableTransaction + Send + Sync,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
    {
        loop {
            let tx = transaction_builder(self.get_latest_blockhash().await?);

            match self
                .send_transaction_with_config_with_custom_expectant::<Expecter, Fut, TxStatus>(
                    tx,
                    config,
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

    async fn send_transaction_with_config_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        config: RpcSendTransactionConfig,
        expectant: &Expecter,
        mut send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
    {
        let span = tracing::span!(Level::TRACE, "send ", tx = %transaction.get_signature());
        let _guard = span.enter();
        if send_ctx.blockhash_validation {
            tracing::trace!(
                "Blockhash {} validation of transaction {:?} started",
                transaction.get_recent_blockhash(),
                transaction.get_signature()
            );
            match self
                .is_blockhash_valid(
                    transaction.get_recent_blockhash(),
                    CommitmentConfig::processed(),
                )
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    return Err(RpcError::ForUser(format!(
                        "Transaction {} blockhash not found by rpc",
                        transaction.get_signature(),
                    ))
                    .into())
                }
                Err(err) => {
                    tracing::error!(
                        "Ignore error via blockhash request of {} transaction: {:?}. Error ignores left: {}",
                        transaction.get_signature(),
                        err,
                        send_ctx.ignorable_errors_count,
                    );
                    return Err(RpcError::ForUser(format!(
                        "Error via transaction {} blockhash requesting",
                        transaction.get_signature(),
                    ))
                    .into());
                }
            }
        }

        let instant = Instant::now();
        loop {
            let signature = self
                .send_transaction_with_config(&transaction, config)
                .await?;
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
                    "Unable to confirm transaction {signature}.",
                ))
                .into());
            }
            time::sleep(send_ctx.confirm_request_pause).await;
        }
    }

    async fn send_transaction_with_custom_expectant<Expecter, Fut, TxStatus>(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        expectant: &Expecter,
        send_ctx: SendContext,
    ) -> Result<(Signature, TxStatus), ClientError>
    where
        Expecter: Send + Sync + Fn(Signature) -> Fut,
        TxStatus: Debug + Send,
        Fut: Send + Future<Output = Result<Option<TxStatus>, ClientError>>,
    {
        self.send_transaction_with_config_with_custom_expectant(
            transaction,
            Default::default(),
            expectant,
            send_ctx,
        )
        .await
    }
}

#[async_trait]
pub trait AsyncSendTransactionWithSimpleStatus: AsyncSendTransaction {
    async fn send_transaction_with_simple_status(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        send_ctx: SendContext,
    ) -> Result<(Signature, Option<TransactionError>), ClientError> {
        self.send_transaction_with_config_with_simple_status(
            transaction,
            Default::default(),
            send_ctx,
        )
        .await
    }

    async fn send_transaction_with_config_with_simple_status(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        config: RpcSendTransactionConfig,
        send_ctx: SendContext,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>;
}

#[async_trait]
impl AsyncSendTransactionWithSimpleStatus for RpcClient {
    async fn send_transaction_with_config_with_simple_status(
        &self,
        transaction: impl SerializableTransaction + Send + Sync,
        config: RpcSendTransactionConfig,
        send_ctx: SendContext,
    ) -> Result<(Signature, Option<TransactionError>), ClientError> {
        self.send_transaction_with_config_with_custom_expectant(
            transaction,
            config,
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
    async fn resend_transaction_with_config_with_simple_status<TransactionBuilder, Tx>(
        &self,
        transaction_builder: TransactionBuilder,
        config: RpcSendTransactionConfig,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>
    where
        TransactionBuilder: Send + Sync + Fn(Hash) -> Tx,
        Tx: SerializableTransaction + Send + Sync;
    async fn resend_transaction_with_simple_status<TransactionBuilder, Tx>(
        &self,
        transaction_builder: TransactionBuilder,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>
    where
        TransactionBuilder: Send + Sync + Fn(Hash) -> Tx,
        Tx: SerializableTransaction + Send + Sync,
    {
        self.resend_transaction_with_config_with_simple_status(
            transaction_builder,
            Default::default(),
            send_ctx,
            resend_count,
        )
        .await
    }
}

#[async_trait]
impl AsyncResendTransactionWithSimpleStatus for RpcClient {
    async fn resend_transaction_with_config_with_simple_status<TransactionBuilder, Tx>(
        &self,
        transaction_builder: TransactionBuilder,
        config: RpcSendTransactionConfig,
        send_ctx: SendContext,
        resend_count: usize,
    ) -> Result<(Signature, Option<TransactionError>), ClientError>
    where
        TransactionBuilder: Send + Sync + Fn(Hash) -> Tx,
        Tx: SerializableTransaction + Send + Sync,
    {
        self.resend_transaction_with_config_with_custom_expectant(
            transaction_builder,
            config,
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
        #[allow(deprecated)]
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

use solana_client::rpc_config::{
    RpcAccountInfoConfig, RpcProgramAccountsConfig, RpcSendTransactionConfig,
};

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

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ClientError(#[from] ClientError),
    #[error(transparent)]
    SignatureParseError(#[from] solana_sdk::signature::ParseSignatureError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturesData {
    pub signature: Signature,
    pub slot: u64,
    pub block_time: Option<UnixTimestamp>,
    pub err: Option<TransactionError>,
}
impl PartialOrd for SignaturesData {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SignaturesData {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.slot.cmp(&other.slot) {
            core::cmp::Ordering::Equal => {}
            ord => return ord,
        }
        match self.block_time.cmp(&other.block_time) {
            core::cmp::Ordering::Equal => {}
            ord => return ord,
        }
        self.signature.cmp(&other.signature)
    }
}

#[async_trait]
pub trait GetTransactionsSignaturesForAddress {
    async fn get_signatures_for_address_with_config(
        &self,
        address: &Pubkey,
        commitment_config: CommitmentConfig,
        until: Option<Signature>,
    ) -> Result<Vec<Signature>, Error> {
        Ok(self
            .get_signatures_data_for_address_with_config(address, commitment_config, until)
            .await?
            .into_iter()
            .filter(|data| data.err.is_none())
            .map(|data| data.signature)
            .collect())
    }
    async fn get_signatures_data_for_address_with_config(
        &self,
        address: &Pubkey,
        commitment_config: CommitmentConfig,
        until: Option<Signature>,
    ) -> Result<BTreeSet<SignaturesData>, Error>;
}

#[async_trait]
impl GetTransactionsSignaturesForAddress for RpcClient {
    #[instrument(skip(self))]
    async fn get_signatures_data_for_address_with_config(
        &self,
        address: &Pubkey,
        commitment_config: CommitmentConfig,
        until: Option<Signature>,
    ) -> Result<BTreeSet<SignaturesData>, Error> {
        let mut all_signatures = BTreeSet::new();
        let mut before = None;

        loop {
            tracing::trace!(
                "Request signature batch, before: {:?}, until: {:?}",
                before,
                until
            );

            let signatures_batch = self
                .get_signatures_for_address_with_config(
                    address,
                    GetConfirmedSignaturesForAddress2Config {
                        before,
                        until,
                        limit: Some(1000),
                        commitment: Some(commitment_config),
                    },
                )
                .await
                .map_err(|err| {
                    tracing::error!(
                        "Error while get signature for address with config: {:?}",
                        err
                    );
                    err
                })?
                .into_iter()
                .map(|tx| {
                    Ok(SignaturesData {
                        signature: tx.signature.parse()?,
                        slot: tx.slot,
                        block_time: tx.block_time,
                        err: tx.err,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?;

            if signatures_batch.is_empty() {
                break;
            }
            tracing::trace!("Batch received: {}", signatures_batch.len());

            before = signatures_batch
                .iter()
                .rev()
                .fold_while(
                    None,
                    |resync_border_tx, signature_data| match resync_border_tx {
                        None => FoldWhile::Continue(Some(signature_data)),
                        Some(resync_border) => {
                            if resync_border.slot != signature_data.slot {
                                FoldWhile::Done(Some(signature_data))
                            } else {
                                FoldWhile::Continue(Some(resync_border))
                            }
                        }
                    },
                )
                .into_inner()
                .map(|d| d.signature);

            let batch_len_before = signatures_batch
                .iter()
                .map(|b| b.slot)
                .all_equal()
                .then_some(all_signatures.len());

            let previous_len = all_signatures.len();
            signatures_batch.into_iter().for_each(|s| {
                all_signatures.insert(s);
            });

            if matches!(
                batch_len_before,
                Some(before_len) if before_len == all_signatures.len()
            ) {
                break;
            }
            if previous_len == all_signatures.len() {
                tracing::warn!("found infinity loop, from: {before:?}, to: {until:?}, returning `all_signatures` as is");
                break;
            }
            tracing::trace!("All signatures: {}", all_signatures.len());
        }

        Ok(all_signatures)
    }
}
