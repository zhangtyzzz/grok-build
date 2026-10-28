//! Session wiring for [`crate::session::text_only_images`]: deciding when the active model is text-only, adapting its requests,
//! and providing the `view_image` vision backend.

use std::sync::Arc;

use xai_grok_sampling_types::ConversationRequest;
use xai_grok_sampling_types::conversation::{ContentPart, ConversationItem};
use xai_grok_tools::implementations::grok_build::VIEW_IMAGE_TOOL_NAME;

use crate::session::acp_session::SessionActor;
use crate::session::text_only_images::VisionViewer;

pub(super) fn request_has_images(request: &ConversationRequest) -> bool {
    request.items.iter().any(|item| match item {
        ConversationItem::User(user) => user
            .content
            .iter()
            .any(|part| matches!(part, ContentPart::Image { .. })),
        ConversationItem::ToolResult(result) => !result.images.is_empty(),
        _ => false,
    })
}

impl SessionActor {
    /// Catalog key for the active model: the physical ref, else the slug.
    async fn active_model_locator(&self) -> Option<(Option<String>, String, String)> {
        let config = self.chat_state_handle.get_sampling_config().await?;
        Some((config.model_ref, config.model, config.base_url))
    }

    /// Whether the active model cannot take image input: declared `supports_images = false`, or learned this session.
    pub(crate) async fn active_model_is_text_only(&self) -> bool {
        let Some((model_ref, model, base_url)) = self.active_model_locator().await else {
            return false;
        };
        let key = model_ref.clone().unwrap_or_else(|| model.clone());
        if self.text_only_images.learned_text_only(&key) {
            return true;
        }
        let models = self.models_manager.models();
        crate::agent::config::find_model_by_locator(
            &models,
            model_ref.as_deref(),
            &model,
            &base_url,
        )
        .is_some_and(|entry| entry.info.supports_images == Some(false))
    }

    /// Remember that the active model rejected image input. Returns `true` the first time.
    pub(crate) async fn learn_active_model_text_only(&self) -> bool {
        let Some((model_ref, model, _)) = self.active_model_locator().await else {
            return false;
        };
        let key = model_ref.unwrap_or(model);
        let first = self.text_only_images.mark_text_only(&key);
        if first {
            tracing::info!(model = %key, "provider rejected image input; routing images through view_image for this session");
        }
        first
    }

    /// Shape `request` for the active model: a text-only model gets image placeholders; any other model loses `view_image`.
    pub(crate) async fn adapt_request_for_image_input(&self, request: &mut ConversationRequest) {
        if !self.active_model_is_text_only().await {
            request
                .tools
                .retain(|tool| tool.name != VIEW_IMAGE_TOOL_NAME);
            return;
        }
        if !request_has_images(request) {
            return;
        }
        let assets_dir = match crate::session::persistence::ensure_owner_only_session_dir(
            &self.session_info,
        ) {
            Ok(dir) => crate::session::image_describe::session_assets_dir(&dir),
            Err(error) => {
                tracing::warn!(%error, "session dir unavailable; image placeholders will carry no path");
                std::env::temp_dir().join("grok-text-only-images")
            }
        };
        let replaced = self.text_only_images.adapt_request(request, &assets_dir);
        tracing::debug!(
            replaced,
            "replaced images with placeholders for a text-only model"
        );
    }

    /// The vision model behind `view_image`, unless it is missing or itself text-only.
    /// Never falls back to the active model: it is the one that cannot see.
    async fn resolve_vision_viewer(&self) -> Option<VisionViewer> {
        let resolved = self
            .resolve_aux_sampler_config(&self.image_description_model)
            .await?;
        let models = self.models_manager.models();
        let vision_is_text_only = crate::agent::config::find_model_by_locator(
            &models,
            resolved.config.model_ref.as_deref(),
            &resolved.config.model,
            &resolved.config.base_url,
        )
        .is_some_and(|entry| entry.info.supports_images == Some(false));
        if vision_is_text_only {
            tracing::warn!(model = %self.image_description_model, "image_description model is marked supports_images = false; view_image disabled");
            return None;
        }
        let active_session_config = self.reconstruct_full_config().await;
        let (model, sampler_config) = crate::agent::config::finalize_image_describe_sampler_config(
            Some(resolved),
            &active_session_config,
            self.client_identifier.clone(),
            Some(self.max_retries),
        );
        match xai_grok_sampler::SamplingClient::new(sampler_config) {
            Ok(client) => Some(VisionViewer { client, model }),
            Err(error) => {
                tracing::warn!(%error, "could not build the view_image sampling client");
                None
            }
        }
    }

    /// Before a turn of a text-only model, install the `view_image` backend (or remove a stale one when no vision model resolves).
    pub(crate) async fn install_image_viewer(&self) {
        if !self.active_model_is_text_only().await {
            return;
        }
        let bridge = self.tool_bridge_handle();
        match self.resolve_vision_viewer().await {
            Some(viewer) => {
                bridge
                    .update_resource(xai_grok_tools::types::resources::ImageViewer(Arc::new(
                        viewer,
                    )))
                    .await;
            }
            None => {
                bridge
                    .update_resources_with(|res| {
                        res.remove::<xai_grok_tools::types::resources::ImageViewer>();
                    })
                    .await;
            }
        }
    }
}
