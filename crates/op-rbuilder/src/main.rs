use clap::Parser;
use generator::BlockPayloadJobGenerator;
use monitoring::Monitoring;
use payload_builder::OpPayloadBuilder as FBPayloadBuilder;
use payload_builder_vanilla::OpPayloadBuilderVanilla;
use reth::{
    builder::{
        components::PayloadServiceBuilder, engine_tree_config::TreeConfig, node::FullNodeTypes,
        BuilderContext, EngineNodeLauncher, Node,
    },
    payload::PayloadBuilderHandle,
    providers::{providers::BlockchainProvider, CanonStateSubscriptions},
    transaction_pool::TransactionPool,
};
use reth_basic_payload_builder::BasicPayloadJobGeneratorConfig;
use reth_node_api::{NodeTypesWithEngine, TxTy};
use reth_optimism_chainspec::OpChainSpec;
use reth_optimism_cli::{chainspec::OpChainSpecParser, Cli};
use reth_optimism_evm::OpEvmConfig;
use reth_optimism_node::{OpEngineTypes, OpNode};
use reth_payload_builder::PayloadBuilderService;
use tx_signer::Signer;

/// CLI argument parsing.
pub mod args;

use reth_optimism_primitives::OpPrimitives;
use reth_transaction_pool::PoolTransaction;

pub mod generator;

mod metrics;
mod monitoring;
pub mod payload_builder;
mod payload_builder_vanilla;
#[cfg(test)]
mod tester;
mod tx_signer;

/// This module contains a lot of unused code.
#[cfg(test)]
#[allow(dead_code)]
mod integration;

#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct CustomPayloadBuilder {
    builder_secret_key: Option<Signer>,
}

impl CustomPayloadBuilder {
    pub fn new(builder_secret_key: Option<Signer>) -> Self {
        Self { builder_secret_key }
    }
}

impl<Node, Pool> PayloadServiceBuilder<Node, Pool> for CustomPayloadBuilder
where
    Node: FullNodeTypes<
        Types: NodeTypesWithEngine<
            Engine = OpEngineTypes,
            ChainSpec = OpChainSpec,
            Primitives = OpPrimitives,
        >,
    >,
    Pool: TransactionPool<Transaction: PoolTransaction<Consensus = TxTy<Node::Types>>>
        + Unpin
        + 'static,
{
    async fn spawn_payload_service(
        self,
        ctx: &BuilderContext<Node>,
        pool: Pool,
    ) -> eyre::Result<PayloadBuilderHandle<<Node::Types as NodeTypesWithEngine>::Engine>> {
        tracing::info!("Spawning a custom payload builder");
        let _fb_builder = FBPayloadBuilder::new(OpEvmConfig::new(ctx.chain_spec()));
        let vanilla_builder = OpPayloadBuilderVanilla::new(
            OpEvmConfig::new(ctx.chain_spec()),
            self.builder_secret_key,
        );
        let payload_job_config = BasicPayloadJobGeneratorConfig::default();

        let payload_generator = BlockPayloadJobGenerator::with_builder(
            ctx.provider().clone(),
            pool,
            ctx.task_executor().clone(),
            payload_job_config,
            // FBPayloadBuilder::new(OpEvmConfig::new(ctx.chain_spec())),
            vanilla_builder,
        );

        let (payload_service, payload_builder) =
            PayloadBuilderService::new(payload_generator, ctx.provider().canonical_state_stream());

        ctx.task_executor()
            .spawn_critical("custom payload builder service", Box::pin(payload_service));

        tracing::info!("Custom payload service started");

        Ok(payload_builder)
    }
}

fn main() {
    Cli::<OpChainSpecParser, args::OpRbuilderArgs>::parse()
        .run(|builder, builder_args| async move {
            let engine_tree_config = TreeConfig::default()
                .with_persistence_threshold(builder_args.engine.persistence_threshold)
                .with_memory_block_buffer_target(builder_args.engine.memory_block_buffer_target);

            let op_node = OpNode::new(builder_args.rollup_args);
            let handle = builder
                .with_types_and_provider::<OpNode, BlockchainProvider<_>>()
                .with_components(
                    op_node
                        .components()
                        .payload(CustomPayloadBuilder::new(builder_args.builder_signer)),
                )
                .with_add_ons(op_node.add_ons())
                .install_exex("monitoring", move |ctx| {
                    let builder_signer = builder_args.builder_signer;
                    if let Some(signer) = &builder_signer {
                        tracing::info!("Builder signer address is set to: {:?}", signer.address);
                    } else {
                        tracing::info!("Builder signer is not set");
                    }
                    async move { Ok(Monitoring::new(ctx, builder_signer).start()) }
                })
                .launch_with_fn(|builder| {
                    let launcher = EngineNodeLauncher::new(
                        builder.task_executor().clone(),
                        builder.config().datadir(),
                        engine_tree_config,
                    );
                    builder.launch_with(launcher)
                })
                .await?;

            handle.node_exit_future.await
        })
        .unwrap();
}
