//! Image input for text-only models.
//!
//! A model whose catalog entry says `supports_images = false`, or whose provider rejected image input earlier in the session,
//! never receives image parts. [`TextOnlyImages::adapt_request`] saves each image under the session's `assets/` directory and
//! swaps it for a text placeholder naming that file. The model inspects the file with the `view_image` tool, which asks the
//! `[models] image_description` vision model ([`VisionViewer`]). The placeholder depends only on the image's content, so every
//! request renders the same text and the prompt cache stays warm.
//!
//! The conversation history keeps the real image parts: switching to a vision model later sends the pixels again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use parking_lot::Mutex;
use xai_grok_sampling_types::ConversationRequest;
use xai_grok_tools::implementations::grok_build::VIEW_IMAGE_TOOL_NAME;

use crate::session::image_describe::{DescribeError, content_fingerprint, describe_user_images};

/// Session-scoped state: models learned to be text-only and saved image files.
#[derive(Debug, Default)]
pub(crate) struct TextOnlyImages {
    /// Catalog keys (model ref, else slug) whose provider rejected image input this session.
    learned_text_only: Mutex<HashSet<String>>,
    /// Image URL -> saved file. Keyed by URL so a request does not re-decode images it already saved.
    saved: Mutex<HashMap<Arc<str>, PathBuf>>,
}

impl TextOnlyImages {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn mark_text_only(&self, model_key: &str) -> bool {
        self.learned_text_only.lock().insert(model_key.to_owned())
    }

    pub(crate) fn learned_text_only(&self, model_key: &str) -> bool {
        self.learned_text_only.lock().contains(model_key)
    }

    /// Replace every image in `request` with a placeholder pointing at a saved copy under `assets_dir`.
    /// Returns how many images were replaced.
    pub(crate) fn adapt_request(
        &self,
        request: &mut ConversationRequest,
        assets_dir: &Path,
    ) -> usize {
        xai_grok_sampling_types::replace_images_with_text(&mut request.items, |url| {
            render_placeholder(self.save(url, assets_dir).as_deref(), url)
        })
    }

    /// The saved file for a `data:` image URL, written once per distinct content.
    fn save(&self, url: &Arc<str>, assets_dir: &Path) -> Option<PathBuf> {
        if let Some(hit) = self.saved.lock().get(url) {
            return Some(hit.clone());
        }
        let (mime, bytes) = decode_data_url(url)?;
        let fingerprint = content_fingerprint(&bytes);
        let name = format!(
            "image-{}.{}",
            fingerprint.get(..16).unwrap_or(&fingerprint),
            extension_for(&mime)
        );
        let path = assets_dir.join(name);
        if !path.exists() {
            if let Err(error) = crate::util::grok_home::create_dir_all_owner_only(assets_dir)
                .and_then(|()| std::fs::write(&path, &bytes))
            {
                tracing::warn!(%error, path = %path.display(), "could not save image for a text-only model");
                return None;
            }
        }
        self.saved.lock().insert(Arc::clone(url), path.clone());
        Some(path)
    }
}

/// `(mime, bytes)` of a base64 `data:` URL.
pub(crate) fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url.strip_prefix("data:")?;
    let (header, data) = rest.split_once(',')?;
    let mime = header.strip_suffix(";base64")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some((mime.to_owned(), bytes))
}

fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "png",
    }
}

/// The text a text-only model sees in place of an image.
pub(crate) fn render_placeholder(path: Option<&Path>, url: &str) -> String {
    let location = match path {
        Some(path) => format!("saved at {}", path.display()),
        None if url.starts_with("data:") => "could not be saved".to_owned(),
        None => format!("at {url}"),
    };
    format!(
        "[Image {location}. You cannot see images directly; call {VIEW_IMAGE_TOOL_NAME} with this path and a specific question to inspect it.]"
    )
}

/// Whether a provider error says the model does not take image input at all (as opposed to one bad image).
/// Matched on the message because providers have no common code for it.
pub(crate) fn is_image_input_unsupported(status: Option<u16>, message: &str) -> bool {
    if !matches!(status, Some(400 | 404 | 415 | 422)) {
        return false;
    }
    let m = message.to_ascii_lowercase();
    const PHRASES: &[&str] = &[
        "unknown variant `image_url`",
        "unknown variant \"image_url\"",
        "support image input",
        "image input is not supported",
        "image input not supported",
        "does not support image",
        "doesn't support image",
        "not support image",
        "images are not supported",
        "image is not supported",
        "vision is not supported",
        "does not support vision",
        "not a multimodal model",
        "does not support multimodal",
    ];
    PHRASES.iter().any(|p| m.contains(p))
        || (m.contains("image_url") && (m.contains("not supported") || m.contains("unsupported")))
}

/// Prompt for one `view_image` question.
fn view_prompt(question: &str) -> String {
    format!(
        "You are the eyes of a coding assistant that cannot see images. Answer its question about the attached image \
         accurately and completely. Transcribe any text, code, error messages, or numbers that matter verbatim. \
         Describe layout and visual details when they are relevant. If the image cannot answer the question, say so.\n\n\
         <question>\n{question}\n</question>"
    )
}

/// The session's vision model behind `view_image`.
#[derive(Debug, Clone)]
pub(crate) struct VisionViewer {
    pub(crate) client: xai_grok_sampler::SamplingClient,
    pub(crate) model: String,
}

impl VisionViewer {
    pub(crate) async fn ask(
        &self,
        bytes: &[u8],
        mime_type: &str,
        prompt: String,
    ) -> Result<String, DescribeError> {
        let url = format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        describe_user_images(self.client.clone(), &self.model, prompt, &[url]).await
    }
}

impl xai_grok_tools::types::resources::ImageViewerBackend for VisionViewer {
    fn view(
        &self,
        bytes: Vec<u8>,
        mime_type: String,
        question: String,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
    {
        Box::pin(async move {
            self.ask(&bytes, &mime_type, view_prompt(&question))
                .await
                .map_err(|e| format!("vision model {}: {e}", self.model))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xai_grok_sampling_types::conversation::{ContentPart, ConversationItem};

    fn png_url() -> Arc<str> {
        let mut buf = Vec::new();
        image::RgbImage::from_pixel(2, 2, image::Rgb([1, 2, 3]))
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        Arc::from(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&buf)
        ))
    }

    fn request_with(url: &Arc<str>) -> ConversationRequest {
        ConversationRequest::from_items(vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: Arc::from("what is this"),
            },
            ContentPart::Image {
                url: Arc::clone(url),
            },
        ])])
    }

    fn user_text(request: &ConversationRequest) -> String {
        let ConversationItem::User(user) = &request.items[0] else {
            panic!("user item")
        };
        match &user.content[1] {
            ContentPart::Text { text } => text.to_string(),
            ContentPart::Image { .. } => panic!("image survived"),
        }
    }

    #[test]
    fn placeholder_is_stable_and_points_at_a_saved_file() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let state = TextOnlyImages::new();
        let url = png_url();
        let mut first = request_with(&url);
        assert_eq!(state.adapt_request(&mut first, &assets), 1);
        let text = user_text(&first);
        assert!(text.contains(VIEW_IMAGE_TOOL_NAME), "{text}");
        let path = text
            .strip_prefix("[Image saved at ")
            .and_then(|rest| rest.split_once(". You cannot"))
            .map(|(path, _)| PathBuf::from(path))
            .unwrap();
        assert!(path.starts_with(&assets) && path.exists());

        let mut second = request_with(&url);
        state.adapt_request(&mut second, &assets);
        assert_eq!(user_text(&second), text, "same image, same placeholder");
    }

    #[test]
    fn unsupported_image_errors_are_recognized() {
        assert!(is_image_input_unsupported(
            Some(400),
            "Failed to deserialize the JSON body into the target type: messages[1]: unknown variant `image_url`, expected `text`"
        ));
        assert!(is_image_input_unsupported(
            Some(404),
            "No endpoints found that support image input"
        ));
        assert!(!is_image_input_unsupported(
            Some(400),
            "Could not process image"
        ));
        assert!(!is_image_input_unsupported(
            Some(500),
            "does not support image input"
        ));
    }
}
