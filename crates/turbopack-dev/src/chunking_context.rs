use anyhow::{bail, Context, Result};
use tracing::Instrument;
use turbo_tasks::{Value, ValueToString, Vc};
use turbo_tasks_fs::FileSystemPath;
use turbopack_core::{
    chunk::{
        availability_info::AvailabilityInfo,
        chunk_group::{make_chunk_group, MakeChunkGroupResult},
        Chunk, ChunkGroupResult, ChunkItem, ChunkableModule, ChunkingContext, EvaluatableAssets,
        ModuleId,
    },
    environment::Environment,
    ident::AssetIdent,
    module::Module,
    output::{OutputAsset, OutputAssets},
};
use turbopack_ecmascript::{
    chunk::{EcmascriptChunk, EcmascriptChunkingContext},
    manifest::{chunk_asset::ManifestAsyncModule, loader_item::ManifestLoaderChunkItem},
};
use turbopack_ecmascript_runtime::RuntimeType;

use crate::ecmascript::{
    chunk::EcmascriptDevChunk,
    evaluate::chunk::EcmascriptDevEvaluateChunk,
    list::asset::{EcmascriptDevChunkList, EcmascriptDevChunkListSource},
};

pub struct DevChunkingContextBuilder {
    chunking_context: DevChunkingContext,
}

impl DevChunkingContextBuilder {
    pub fn hot_module_replacement(mut self) -> Self {
        self.chunking_context.enable_hot_module_replacement = true;
        self
    }

    pub fn asset_base_path(mut self, asset_base_path: Vc<Option<String>>) -> Self {
        self.chunking_context.asset_base_path = asset_base_path;
        self
    }

    pub fn chunk_base_path(mut self, chunk_base_path: Vc<Option<String>>) -> Self {
        self.chunking_context.chunk_base_path = chunk_base_path;
        self
    }

    pub fn reference_chunk_source_maps(mut self, source_maps: bool) -> Self {
        self.chunking_context.reference_chunk_source_maps = source_maps;
        self
    }

    pub fn reference_css_chunk_source_maps(mut self, source_maps: bool) -> Self {
        self.chunking_context.reference_css_chunk_source_maps = source_maps;
        self
    }

    pub fn runtime_type(mut self, runtime_type: RuntimeType) -> Self {
        self.chunking_context.runtime_type = runtime_type;
        self
    }

    pub fn build(self) -> Vc<DevChunkingContext> {
        DevChunkingContext::new(Value::new(self.chunking_context))
    }
}

/// A chunking context for development mode.
/// It uses readable filenames and module ids to improve development.
/// It also uses a chunking heuristic that is incremental and cacheable.
/// It splits "node_modules" separately as these are less likely to change
/// during development
#[turbo_tasks::value(serialization = "auto_for_input")]
#[derive(Debug, Clone, Hash, PartialOrd, Ord)]
pub struct DevChunkingContext {
    /// This path get stripped off of chunk paths before generating output asset
    /// paths.
    context_path: Vc<FileSystemPath>,
    /// This path is used to compute the url to request chunks or assets from
    output_root: Vc<FileSystemPath>,
    /// Chunks are placed at this path
    chunk_root_path: Vc<FileSystemPath>,
    /// Chunks reference source maps assets
    reference_chunk_source_maps: bool,
    /// Css chunks reference source maps assets
    reference_css_chunk_source_maps: bool,
    /// Static assets are placed at this path
    asset_root_path: Vc<FileSystemPath>,
    /// Base path that will be prepended to all chunk URLs when loading them.
    /// This path will not appear in chunk paths or chunk data.
    chunk_base_path: Vc<Option<String>>,
    /// URL prefix that will be prepended to all static asset URLs when loading
    /// them.
    asset_base_path: Vc<Option<String>>,
    /// Enable HMR for this chunking
    enable_hot_module_replacement: bool,
    /// The environment chunks will be evaluated in.
    environment: Vc<Environment>,
    /// The kind of runtime to include in the output.
    runtime_type: RuntimeType,
}

impl DevChunkingContext {
    pub fn builder(
        context_path: Vc<FileSystemPath>,
        output_root: Vc<FileSystemPath>,
        chunk_root_path: Vc<FileSystemPath>,
        asset_root_path: Vc<FileSystemPath>,
        environment: Vc<Environment>,
    ) -> DevChunkingContextBuilder {
        DevChunkingContextBuilder {
            chunking_context: DevChunkingContext {
                context_path,
                output_root,
                chunk_root_path,
                reference_chunk_source_maps: true,
                reference_css_chunk_source_maps: true,
                asset_root_path,
                chunk_base_path: Default::default(),
                asset_base_path: Default::default(),
                enable_hot_module_replacement: false,
                environment,
                runtime_type: Default::default(),
            },
        }
    }
}

impl DevChunkingContext {
    /// Returns the kind of runtime to include in output chunks.
    ///
    /// This is defined directly on `DevChunkingContext` so it is zero-cost when
    /// `RuntimeType` has a single variant.
    pub fn runtime_type(&self) -> RuntimeType {
        self.runtime_type
    }

    /// Returns the asset base path.
    pub fn chunk_base_path(&self) -> Vc<Option<String>> {
        self.chunk_base_path
    }
}

#[turbo_tasks::value_impl]
impl DevChunkingContext {
    #[turbo_tasks::function]
    fn new(this: Value<DevChunkingContext>) -> Vc<Self> {
        this.into_value().cell()
    }

    #[turbo_tasks::function]
    fn generate_evaluate_chunk(
        self: Vc<Self>,
        ident: Vc<AssetIdent>,
        other_chunks: Vc<OutputAssets>,
        evaluatable_assets: Vc<EvaluatableAssets>,
    ) -> Vc<Box<dyn OutputAsset>> {
        Vc::upcast(EcmascriptDevEvaluateChunk::new(
            self,
            ident,
            other_chunks,
            evaluatable_assets,
        ))
    }

    #[turbo_tasks::function]
    fn generate_chunk_list_register_chunk(
        self: Vc<Self>,
        ident: Vc<AssetIdent>,
        evaluatable_assets: Vc<EvaluatableAssets>,
        other_chunks: Vc<OutputAssets>,
        source: Value<EcmascriptDevChunkListSource>,
    ) -> Vc<Box<dyn OutputAsset>> {
        Vc::upcast(EcmascriptDevChunkList::new(
            self,
            ident,
            evaluatable_assets,
            other_chunks,
            source,
        ))
    }

    #[turbo_tasks::function]
    async fn generate_chunk(
        self: Vc<Self>,
        chunk: Vc<Box<dyn Chunk>>,
    ) -> Result<Vc<Box<dyn OutputAsset>>> {
        Ok(
            if let Some(ecmascript_chunk) =
                Vc::try_resolve_downcast_type::<EcmascriptChunk>(chunk).await?
            {
                Vc::upcast(EcmascriptDevChunk::new(self, ecmascript_chunk))
            } else if let Some(output_asset) =
                Vc::try_resolve_sidecast::<Box<dyn OutputAsset>>(chunk).await?
            {
                output_asset
            } else {
                bail!("Unable to generate output asset for chunk");
            },
        )
    }
}

#[turbo_tasks::value_impl]
impl ChunkingContext for DevChunkingContext {
    #[turbo_tasks::function]
    fn context_path(&self) -> Vc<FileSystemPath> {
        self.context_path
    }

    #[turbo_tasks::function]
    fn output_root(&self) -> Vc<FileSystemPath> {
        self.output_root
    }

    #[turbo_tasks::function]
    fn environment(&self) -> Vc<Environment> {
        self.environment
    }

    #[turbo_tasks::function]
    async fn chunk_path(
        &self,
        ident: Vc<AssetIdent>,
        extension: String,
    ) -> Result<Vc<FileSystemPath>> {
        let root_path = self.chunk_root_path;
        let name = ident.output_name(self.context_path, extension).await?;
        Ok(root_path.join(name.clone_value()))
    }

    #[turbo_tasks::function]
    async fn asset_url(self: Vc<Self>, ident: Vc<AssetIdent>) -> Result<Vc<String>> {
        let this = self.await?;
        let asset_path = ident.path().await?.to_string();
        let asset_path = asset_path
            .strip_prefix(&format!("{}/", this.output_root.await?.path))
            .context("expected output_root to contain asset path")?;

        Ok(Vc::cell(format!(
            "{}{}",
            this.asset_base_path
                .await?
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("/"),
            asset_path
        )))
    }

    #[turbo_tasks::function]
    async fn reference_chunk_source_maps(
        &self,
        chunk: Vc<Box<dyn OutputAsset>>,
    ) -> Result<Vc<bool>> {
        let mut source_maps = self.reference_chunk_source_maps;
        let path = chunk.ident().path().await?;
        let extension = path.extension_ref().unwrap_or_default();
        #[allow(clippy::single_match, reason = "future extensions")]
        match extension {
            ".css" => {
                source_maps = self.reference_css_chunk_source_maps;
            }
            _ => {}
        }
        Ok(Vc::cell(source_maps))
    }

    #[turbo_tasks::function]
    async fn can_be_in_same_chunk(
        &self,
        asset_a: Vc<Box<dyn Module>>,
        asset_b: Vc<Box<dyn Module>>,
    ) -> Result<Vc<bool>> {
        let parent_dir = asset_a.ident().path().parent().await?;

        let path = asset_b.ident().path().await?;
        if let Some(rel_path) = parent_dir.get_path_to(&path) {
            if !rel_path.starts_with("node_modules/") && !rel_path.contains("/node_modules/") {
                return Ok(Vc::cell(true));
            }
        }

        Ok(Vc::cell(false))
    }

    #[turbo_tasks::function]
    async fn asset_path(
        &self,
        content_hash: String,
        original_asset_ident: Vc<AssetIdent>,
    ) -> Result<Vc<FileSystemPath>> {
        let source_path = original_asset_ident.path().await?;
        let basename = source_path.file_name();
        let asset_path = match source_path.extension_ref() {
            Some(ext) => format!(
                "{basename}.{content_hash}.{ext}",
                basename = &basename[..basename.len() - ext.len() - 1],
                content_hash = &content_hash[..8]
            ),
            None => format!(
                "{basename}.{content_hash}",
                content_hash = &content_hash[..8]
            ),
        };
        Ok(self.asset_root_path.join(asset_path))
    }

    #[turbo_tasks::function]
    fn is_hot_module_replacement_enabled(&self) -> Vc<bool> {
        Vc::cell(self.enable_hot_module_replacement)
    }

    #[turbo_tasks::function]
    async fn chunk_group(
        self: Vc<Self>,
        module: Vc<Box<dyn ChunkableModule>>,
        availability_info: Value<AvailabilityInfo>,
    ) -> Result<Vc<ChunkGroupResult>> {
        let span = tracing::info_span!("chunking", module = *module.ident().to_string().await?);
        async move {
            let MakeChunkGroupResult {
                chunks,
                availability_info,
            } = make_chunk_group(
                Vc::upcast(self),
                [Vc::upcast(module)],
                availability_info.into_value(),
            )
            .await?;

            let mut assets: Vec<Vc<Box<dyn OutputAsset>>> = chunks
                .iter()
                .map(|chunk| self.generate_chunk(*chunk))
                .collect();

            assets.push(self.generate_chunk_list_register_chunk(
                module.ident(),
                EvaluatableAssets::empty(),
                Vc::cell(assets.clone()),
                Value::new(EcmascriptDevChunkListSource::Dynamic),
            ));

            // Resolve assets
            for asset in assets.iter_mut() {
                *asset = asset.resolve().await?;
            }

            Ok(ChunkGroupResult {
                assets: Vc::cell(assets),
                availability_info,
            }
            .cell())
        }
        .instrument(span)
        .await
    }

    #[turbo_tasks::function]
    async fn evaluated_chunk_group(
        self: Vc<Self>,
        ident: Vc<AssetIdent>,
        evaluatable_assets: Vc<EvaluatableAssets>,
        availability_info: Value<AvailabilityInfo>,
    ) -> Result<Vc<ChunkGroupResult>> {
        let span = {
            let ident = ident.to_string().await?;
            tracing::info_span!("chunking", chunking_type = "evaluated", ident = *ident)
        };
        async move {
            let availability_info = availability_info.into_value();

            let evaluatable_assets_ref = evaluatable_assets.await?;

            // TODO this collect is unnecessary, but it hits a compiler bug when it's not
            // used
            let entries = evaluatable_assets_ref
                .iter()
                .map(|&evaluatable| Vc::upcast(evaluatable))
                .collect::<Vec<_>>();

            let MakeChunkGroupResult {
                chunks,
                availability_info,
            } = make_chunk_group(Vc::upcast(self), entries, availability_info).await?;

            let mut assets: Vec<Vc<Box<dyn OutputAsset>>> = chunks
                .iter()
                .map(|chunk| self.generate_chunk(*chunk))
                .collect();

            let other_assets = Vc::cell(assets.clone());

            assets.push(self.generate_chunk_list_register_chunk(
                ident,
                evaluatable_assets,
                other_assets,
                Value::new(EcmascriptDevChunkListSource::Entry),
            ));

            assets.push(self.generate_evaluate_chunk(ident, other_assets, evaluatable_assets));

            // Resolve assets
            for asset in assets.iter_mut() {
                *asset = asset.resolve().await?;
            }

            Ok(ChunkGroupResult {
                assets: Vc::cell(assets),
                availability_info,
            }
            .cell())
        }
        .instrument(span)
        .await
    }

    #[turbo_tasks::function]
    fn async_loader_chunk_item(
        self: Vc<Self>,
        module: Vc<Box<dyn ChunkableModule>>,
        availability_info: Value<AvailabilityInfo>,
    ) -> Vc<Box<dyn ChunkItem>> {
        let manifest_asset = ManifestAsyncModule::new(module, Vc::upcast(self), availability_info);
        Vc::upcast(ManifestLoaderChunkItem::new(
            manifest_asset,
            Vc::upcast(self),
        ))
    }

    #[turbo_tasks::function]
    fn async_loader_chunk_item_id(
        self: Vc<Self>,
        module: Vc<Box<dyn ChunkableModule>>,
    ) -> Vc<ModuleId> {
        self.chunk_item_id_from_ident(ManifestLoaderChunkItem::asset_ident_for(module))
    }
}

#[turbo_tasks::value_impl]
impl EcmascriptChunkingContext for DevChunkingContext {
    #[turbo_tasks::function]
    fn has_react_refresh(&self) -> Vc<bool> {
        Vc::cell(true)
    }
}
