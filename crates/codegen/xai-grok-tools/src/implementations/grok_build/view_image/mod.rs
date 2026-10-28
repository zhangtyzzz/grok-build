//! `view_image` — let a text-only model inspect an image through an auxiliary vision model.
//!
//! The session swaps every image a text-only model would receive for a text placeholder naming a file path.
//! This tool reads that file (or any image path the model knows), sends it with the model's question to the
//! session's vision model through the [`ImageViewer`] resource, and returns the answer as text.
//! The session filters the tool out of requests whose model can see images itself.

use std::path::{Path, PathBuf};

use crate::types::resources::{Cwd, DisplayCwd, ImageViewer, resolve_model_path};
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::util::image_compress::{FilterType, ReEncodeParams, re_encode_under_limit};

pub const VIEW_IMAGE_TOOL_NAME: &str = "view_image";

/// Files above this are refused before decoding.
const MAX_SOURCE_BYTES: u64 = 30 * 1024 * 1024;

/// What the vision model receives; larger or unusual images are re-encoded to fit.
const SEND_PARAMS: ReEncodeParams = ReEncodeParams {
    max_bytes: 4 * 1024 * 1024,
    max_side_px: 2048,
    max_pixels: 2048 * 2048,
    min_side_px: 256,
    quality_steps: &[90, 80, 70, 60],
    filter: FilterType::CatmullRom,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ViewImageInput {
    #[schemars(
        description = "Path of the image file (absolute, or relative to the working directory). Use the path given in an image placeholder."
    )]
    pub path: String,
    #[schemars(
        description = "What you need to know about the image. Be specific (e.g. 'transcribe the error text', 'list the UI elements and their layout'). Omit for a general description."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewImageOutput {
    pub path: String,
    pub answer: String,
}

impl xai_tool_runtime::ToolOutput for ViewImageOutput {}

impl From<ViewImageOutput> for crate::types::output::ToolOutput {
    fn from(output: ViewImageOutput) -> Self {
        Self::Text(format!("Image {}:\n{}", output.path, output.answer).into())
    }
}

#[derive(Debug, Default)]
pub struct ViewImageTool;

impl crate::types::tool_metadata::ToolMetadata for ViewImageTool {
    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::GrokBuild
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn lock_path_param(&self) -> Option<&'static str> {
        Some("path")
    }

    fn description_template(&self) -> &str {
        "Look at an image file. You cannot see images directly: an image in the conversation appears as a placeholder with its file path. Call this tool with that path (or the path of any image file, such as a screenshot or a PNG in the repository) and a specific question; a vision model looks at the image and answers in text. Ask focused questions and call it again with a new question when you need more detail; the answer only covers what you asked."
    }
}

fn tool_id() -> xai_tool_protocol::ToolId {
    xai_tool_protocol::ToolId::new(VIEW_IMAGE_TOOL_NAME).expect("valid tool id")
}

fn exec_error(detail: impl Into<String>) -> xai_tool_runtime::ToolError {
    xai_tool_runtime::ToolError::execution(tool_id(), detail.into())
}

/// The MIME type of a format the vision request can carry as-is.
fn passthrough_mime(format: image::ImageFormat) -> Option<&'static str> {
    match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// Read `path` and return bytes the vision model accepts: passthrough when small and common, else re-encoded.
pub(crate) fn load_image_for_vision(path: &Path) -> Result<(Vec<u8>, String), String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "{} is {} bytes; images over {MAX_SOURCE_BYTES} bytes are not supported",
            path.display(),
            meta.len()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let format = image::guess_format(&bytes)
        .map_err(|_| format!("{} is not a recognized image file", path.display()))?;
    if let Some(mime) = passthrough_mime(format)
        && bytes.len() <= SEND_PARAMS.max_bytes
        && image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
            .into_dimensions()
            .is_ok_and(|(w, h)| !SEND_PARAMS.exceeds_dimension_caps(w, h))
    {
        return Ok((bytes, mime.to_owned()));
    }
    let decoded = image::load_from_memory_with_format(&bytes, format)
        .map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
    let (out, _, _, mime) = re_encode_under_limit(&decoded, &SEND_PARAMS)
        .map_err(|e| format!("cannot prepare {}: {e}", path.display()))?;
    Ok((out, mime.to_owned()))
}

impl xai_tool_runtime::Tool for ViewImageTool {
    type Args = ViewImageInput;
    type Output = ViewImageOutput;

    fn id(&self) -> xai_tool_protocol::ToolId {
        tool_id()
    }

    fn description(
        &self,
        _ctx: &xai_tool_runtime::ListToolsContext,
    ) -> xai_tool_types::ToolDescription {
        xai_tool_types::ToolDescription::new(
            VIEW_IMAGE_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> xai_tool_protocol::ToolCapabilities {
        xai_tool_protocol::ToolCapabilities {
            is_read_only: true,
            tool_scope: Some(xai_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "tool.view_image", skip_all)]
    async fn run(
        &self,
        ctx: xai_tool_runtime::ToolCallContext,
        input: ViewImageInput,
    ) -> Result<ViewImageOutput, xai_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;
        let (viewer, cwd, display_cwd): (ImageViewer, PathBuf, Option<PathBuf>) = {
            let res = resources.lock().await;
            let viewer = res.get::<ImageViewer>().cloned().ok_or_else(|| {
                exec_error(
                    "no vision model is available to look at images; set `[models] image_description` in config.toml to a model that accepts images",
                )
            })?;
            (
                viewer,
                res.require::<Cwd>()?.0.clone(),
                res.get::<DisplayCwd>().map(|d| d.0.clone()),
            )
        };
        let path = resolve_model_path(&cwd, display_cwd.as_deref(), &input.path);
        let load_path = path.clone();
        let (bytes, mime_type) =
            tokio::task::spawn_blocking(move || load_image_for_vision(&load_path))
                .await
                .map_err(|e| exec_error(format!("image load task failed: {e}")))?
                .map_err(exec_error)?;
        let question = input
            .question
            .as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .unwrap_or("Describe this image in detail.")
            .to_owned();
        let answer = viewer
            .0
            .view(bytes, mime_type, question)
            .await
            .map_err(exec_error)?;
        Ok(ViewImageOutput {
            path: path.display().to_string(),
            answer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::resources::{ImageViewerBackend, Resources};
    use crate::types::tool_metadata::test_ctx_with_call_id;
    use std::sync::Arc;

    #[derive(Debug)]
    struct EchoViewer;

    impl ImageViewerBackend for EchoViewer {
        fn view(
            &self,
            bytes: Vec<u8>,
            mime_type: String,
            question: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + '_>>
        {
            Box::pin(async move { Ok(format!("{mime_type} {} bytes: {question}", bytes.len())) })
        }
    }

    fn write_png(dir: &Path, w: u32, h: u32) -> PathBuf {
        let path = dir.join("shot.png");
        image::RgbImage::from_pixel(w, h, image::Rgb([200, 10, 10]))
            .save(&path)
            .unwrap();
        path
    }

    fn resources_with(cwd: &Path, viewer: bool) -> Resources {
        let mut resources = Resources::new();
        resources.insert(Cwd(cwd.to_path_buf()));
        if viewer {
            resources.insert(ImageViewer(Arc::new(EchoViewer)));
        }
        resources
    }

    #[tokio::test]
    async fn relative_path_is_sent_with_the_question() {
        let dir = tempfile::tempdir().unwrap();
        write_png(dir.path(), 4, 4);
        let out = xai_tool_runtime::Tool::run(
            &ViewImageTool,
            test_ctx_with_call_id(resources_with(dir.path(), true).into_shared(), "c1"),
            ViewImageInput {
                path: "shot.png".into(),
                question: Some("what color?".into()),
            },
        )
        .await
        .unwrap();
        assert!(out.answer.starts_with("image/png "), "{}", out.answer);
        assert!(out.answer.ends_with(": what color?"));
        assert!(out.path.ends_with("shot.png"));
    }

    #[tokio::test]
    async fn missing_viewer_names_the_config_key() {
        let dir = tempfile::tempdir().unwrap();
        write_png(dir.path(), 4, 4);
        let err = xai_tool_runtime::Tool::run(
            &ViewImageTool,
            test_ctx_with_call_id(resources_with(dir.path(), false).into_shared(), "c2"),
            ViewImageInput {
                path: "shot.png".into(),
                question: None,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("image_description"), "{err}");
    }

    #[test]
    fn oversized_image_is_downscaled() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_png(dir.path(), 3000, 100);
        let (bytes, _) = load_image_for_vision(&path).unwrap();
        let (w, h) = image::load_from_memory(&bytes)
            .unwrap()
            .into_rgb8()
            .dimensions();
        assert!(w <= 2048 && h <= 2048, "{w}x{h}");
    }

    #[test]
    fn non_image_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "hello").unwrap();
        let err = load_image_for_vision(&path).unwrap_err();
        assert!(err.contains("not a recognized image"), "{err}");
    }
}
