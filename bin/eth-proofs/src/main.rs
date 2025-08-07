use std::sync::Arc;

use alloy_provider::{Provider, ProviderBuilder, WsConnect};
use clap::Parser;
use cli::Args;
use eth_proofs::EthProofsClient;
use futures::StreamExt;
use rsp_host_executor::{
    create_eth_block_execution_strategy_factory, BlockExecutor, EthExecutorComponents, FullExecutor,
};
use rsp_provider::create_provider;
use sp1_sdk::{include_elf, ProverClient};
use tracing::{error, info};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod cli;

mod eth_proofs;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Initialize the environment variables.
    dotenv::dotenv().ok();

    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }

    // Initialize the logger.
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(
            EnvFilter::from_default_env()
                .add_directive("sp1_core_machine=warn".parse().unwrap())
                .add_directive("sp1_core_executor=warn".parse().unwrap())
                .add_directive("sp1_prover=warn".parse().unwrap()),
        )
        .init();

    // Parse the command line arguments.
    let args = Args::parse();
    let config = args.as_config().await?;

    let elf = include_elf!("rsp-client").to_vec();
    let block_execution_strategy_factory =
        create_eth_block_execution_strategy_factory(&config.genesis, None);

    let eth_proofs_client = EthProofsClient::new(
        args.eth_proofs_cluster_id,
        args.eth_proofs_endpoint,
        args.eth_proofs_api_token,
    );

    let ws = WsConnect::new(args.ws_rpc_url);
    let ws_provider = ProviderBuilder::new().connect_ws(ws).await?;
    let http_provider = create_provider(args.http_rpc_url);

    // Subscribe to block headers.
    let subscription = ws_provider.subscribe_blocks().await?;
    let mut stream = subscription.into_stream();

    let builder = ProverClient::builder().cuda();
    let client = if let Some(endpoint) = &args.moongate_endpoint {
        builder.server(endpoint).build()
    } else {
        builder.build()
    };

    let client = Arc::new(client);

    let executor = FullExecutor::<EthExecutorComponents<_, _>, _>::try_new(
        http_provider.clone(),
        elf,
        block_execution_strategy_factory,
        client,
        eth_proofs_client,
        config,
    )
    .await?;

    info!("Latest ETH block number: {}", http_provider.get_block_number().await?);

    while let Some(header) = stream.next().await {
        // // Wait for the block to be avaliable in the HTTP provider
        let block_number = executor.wait_for_block(header.number).await?;
        info!("Processing block: {}", block_number);
        let last_two_digits = format!("{:02}", block_number % 100);

        info!("Last two digits of block number: {}", last_two_digits);
        if last_two_digits != "00" {
            info!("Skipping block {} as it does not end with '00'", block_number);
            continue;
        }
        if let Err(err) = executor.execute(header.number).await {
            let error_message = format!("Error handling block number {}: {err}", header.number);
            error!(error_message);
        }
    }

    Ok(())
}