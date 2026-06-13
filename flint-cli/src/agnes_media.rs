//! Agnes AI media generation tools (video + image).
//!
//! - **VideoGen**: async text-to-video / image-to-video generation via `/v1/videos`
//!   - Model: `agnes-video-v2.0`
//!   - Base URL: `https://apihub.agnes-ai.com`
//! - **ImageGen**: sync text-to-image generation via `/v1/images/generations`
//!   - Model: `agnes-image-2.1-flash`
//!   - Base URL: `https://apihub.agnes-ai.com`

use anyhow::Result;
use async_trait::async_trait;
use flint_agent::Tool;
use flint_types::{ToolDefinition, ToolOutput};
use std::time::Duration;

// ── Common ─────────────────────────────────────────────────────────────────

fn api_key() -> Result<String> {
    // Check process env first
    if let Ok(key) = std::env::var("AGNES_API_KEY") {
        if !key.is_empty() {
            return Ok(key);
        }
    }
    // Fallback: read from ~/.flint/.env file
    if let Some(home) = dirs::home_dir() {
        let env_path = home.join(".flint").join(".env");
        if let Ok(content) = std::fs::read_to_string(&env_path) {
            for line in content.lines() {
                let line = line.trim();
                if let Some((k, v)) = line.split_once('=') {
                    if k.trim() == "AGNES_API_KEY" {
                        let val = v.trim().trim_matches('"').trim_matches('\'');
                        if !val.is_empty() {
                            return Ok(val.to_string());
                        }
                    }
                }
            }
        }
    }
    Err(anyhow::anyhow!(
        "AGNES_API_KEY not found. \
         Set it in environment or in ~/.flint/.env file. \
         Get one at https://agnes-ai.com"
    ))
}

fn base_url() -> String {
    std::env::var("AGNES_BASE_URL")
        .unwrap_or_else(|_| "https://apihub.agnes-ai.com".to_string())
}

fn format_elapsed_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else {
        format!("{}m{}s", secs / 60, secs % 60)
    }
}

// ── Video URL extraction ───────────────────────────────────────────────────

/// Extract video URL from a task response.
/// The Agnes API returns the video URL in `remixed_from_video_id` field.
fn extract_video_url(task: &VideoTaskResponse) -> String {
    // 1. remixed_from_video_id — actual video URL from Agnes API
    if let Some(ref u) = task.remixed_from_video_id {
        if !u.is_empty() && u.starts_with("http") { return u.clone(); }
    }
    // 2. Direct URL fields
    if let Some(ref u) = task.video_url {
        if !u.is_empty() { return u.clone(); }
    }
    if let Some(ref u) = task.url {
        if !u.is_empty() { return u.clone(); }
    }
    // 3. Nested in output/data/result objects
    let nested_objs = [&task.output, &task.data, &task.result];
    for obj in nested_objs.iter().filter_map(|o| o.as_ref()) {
        for key in &["url", "video_url", "video", "download_url"] {
            if let Some(u) = obj[key].as_str() {
                if !u.is_empty() { return u.to_string(); }
            }
        }
    }
    // 4. Fallback: video_id
    if let Some(ref id) = task.video_id {
        if !id.is_empty() { return id.clone(); }
    }
    "(no URL in response)".to_string()
}

/// Format optional video metadata (size, duration).
fn format_video_meta(task: &VideoTaskResponse) -> String {
    let mut parts = Vec::new();
    if let Some(ref s) = task.size {
        parts.push(format!("Size: {}", s));
    }
    if let Some(ref s) = task.seconds {
        parts.push(format!("Duration: {}s", s));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("\n{}", parts.join(", "))
    }
}

/// Video task response — matches Agnes AI agnesapi?video_id= endpoint.
#[derive(serde::Deserialize)]
struct VideoTaskResponse {
    status: Option<String>,
    progress: Option<f64>,
    video_id: Option<String>,
    remixed_from_video_id: Option<String>,
    video_url: Option<String>,
    url: Option<String>,
    output: Option<serde_json::Value>,
    data: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    error: Option<serde_json::Value>,
    completed_at: Option<serde_json::Value>,
    seconds: Option<String>,
    size: Option<String>,
}

// ── VideoGen Tool ──────────────────────────────────────────────────────────

/// Async video generation tool.
///
/// Workflow:
/// 1. POST /v1/videos → returns video_id
/// 2. GET /agnesapi?video_id=<VIDEO_ID> → polls until done, returns video URL
///
/// Supports text-to-video and image-to-video.
pub struct VideoGenTool;

#[async_trait]
impl Tool for VideoGenTool {
    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(600)) // 10 minutes
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "video_gen".into(),
            description: "Generate a video from text prompt or image using Agnes AI \
                (agnes-video-v2.0). Async: submits task, polls via video_id, returns URL. \
                Input: {\"prompt\": \"...\", \"image_url\": \"...\" (optional), \
                \"num_frames\": 129, \"fps\": 8}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Text description of the video to generate"
                    },
                    "image_url": {
                        "type": "string",
                        "description": "URL of an input image for image-to-video (optional)"
                    },
                    "num_frames": {
                        "type": "integer",
                        "description": "Number of frames. Must follow rule 8n+1 (e.g. 1, 9, 17, ..., 129, 441). Default 129.",
                        "default": 129
                    },
                    "fps": {
                        "type": "integer",
                        "description": "Frames per second. Range 1-60. Default 8.",
                        "default": 8
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &flint_agent::ToolContext) -> Result<ToolOutput> {
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter 'prompt'"))?;
        let image_url = input["image_url"].as_str();
        let num_frames: i64 = input["num_frames"].as_i64().unwrap_or(129);
        let fps: i64 = input["fps"].as_i64().unwrap_or(8);

        let api_key = api_key()?;
        let base = base_url();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600)) // 10 minutes
            .build()?;

        // Build request body
        let mut body = serde_json::json!({
            "model": "agnes-video-v2.0",
            "prompt": prompt,
            "num_frames": num_frames,
            "fps": fps,
        });
        if let Some(img) = image_url {
            body["image"] = serde_json::json!(img);
        }

        // Step 1: Submit video generation task
        let url = format!("{}/v1/videos", base);
        let resp = client.post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolOutput::error(format!(
                "video generation failed (HTTP {}): {}",
                status, text
            )));
        }

        // Parse response for video_id (preferred) or task_id
        #[derive(serde::Deserialize)]
        struct SubmitResponse {
            task_id: Option<String>,
            id: Option<String>,
            video_id: Option<String>,
            error: Option<serde_json::Value>,
        }
        let submit: SubmitResponse = resp.json().await?;
        let video_id = submit.video_id.or(submit.id).ok_or_else(|| {
            anyhow::anyhow!("video generation submitted but no video_id returned")
        })?;

        // Step 2: Poll for completion using video_id (recommended by API docs)
        // Endpoint: GET https://apihub.agnes-ai.com/agnesapi?video_id=<VIDEO_ID>
        let retrieve_url = format!("{}/agnesapi?video_id={}", base, video_id);
        let mut attempts = 0u32;
        let max_attempts = 60; // 10 minutes with 10s interval
        let poll_interval = std::time::Duration::from_secs(10);
        let poll_start = std::time::Instant::now();
        let mut last_reported_progress: i64 = -1;

        loop {
            if attempts >= max_attempts {
                return Ok(ToolOutput::text(format!(
                    "Video {} still processing after {}. \
                     Check: GET {}/agnesapi?video_id={}",
                    video_id,
                    format_elapsed_secs(poll_start.elapsed().as_secs()),
                    base, video_id
                )));
            }

            let resp = client.get(&retrieve_url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await?;

            let poll_status = resp.status();
            if !poll_status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Ok(ToolOutput::error(format!(
                    "failed to poll video {}: HTTP {} {}",
                    video_id, poll_status, text
                )));
            }

            // Get raw response text for flexible parsing
            let resp_text = resp.text().await?;

            let task: VideoTaskResponse = match serde_json::from_str(&resp_text) {
                Ok(t) => t,
                Err(_) => {
                    return Ok(ToolOutput::error(format!(
                        "Failed to parse poll response for video {}:\n{}",
                        video_id, &resp_text[..resp_text.len().min(500)]
                    )));
                }
            };

            let status_str = task.status.as_deref().unwrap_or("unknown");
            let progress = task.progress.map(|p| p as i64).unwrap_or(0);

            // Broad completion detection — API may use any of these
            let is_completed = matches!(
                status_str,
                "completed" | "succeeded" | "done" | "finished" | "success"
            ) || progress >= 100 || task.completed_at.is_some();

            let is_failed = matches!(
                status_str,
                "failed" | "error" | "cancelled" | "canceled"
            );

            if is_completed {
                let video_url = extract_video_url(&task);
                let meta = format_video_meta(&task);
                return Ok(ToolOutput::text(format!(
                    "✅ Video generated!\nURL: {}\nID: {}{}",
                    video_url, video_id, meta
                )));
            }

            if is_failed {
                let err_msg = task.error
                    .map(|e| match e {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                return Ok(ToolOutput::error(format!(
                    "Video generation failed ({}): {}",
                    status_str, err_msg
                )));
            }

            // Log progress only when it changes and is > 0
            if progress > 0 && progress != last_reported_progress {
                last_reported_progress = progress;
                let elapsed = format_elapsed_secs(poll_start.elapsed().as_secs());
                eprintln!(
                    "\x1b[90m  [video_gen] {}% ({} elapsed)\x1b[0m",
                    progress, elapsed
                );
                use std::io::Write;
                let _ = std::io::stderr().flush();
            }

            attempts += 1;
            tokio::time::sleep(poll_interval).await;
        }
    }
}

// ── ImageGen Tool ──────────────────────────────────────────────────────────

/// Synchronous image generation tool.
///
/// Uses the Agnes AI image generation model (agnes-image-2.1-flash).
/// POST /v1/images/generations → returns image URL directly.
pub struct ImageGenTool;

#[async_trait]
impl Tool for ImageGenTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "image_gen".into(),
            description: "Generate an image from a text prompt using \
                the Agnes AI image generation model (agnes-image-2.1-flash). \
                Input: {\"prompt\": \"...\", \"size\": \"1024x1024\"}"
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Text description of the image to generate"
                    },
                    "size": {
                        "type": "string",
                        "description": "Image size (e.g. '1024x1024', '1280x720'). Default '1024x1024'.",
                        "default": "1024x1024"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of images to generate. Default 1.",
                        "default": 1
                    }
                },
                "required": ["prompt"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value, _ctx: &flint_agent::ToolContext) -> Result<ToolOutput> {
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing required parameter 'prompt'"))?;
        let size = input["size"].as_str().unwrap_or("1024x1024");
        let count: i64 = input["count"].as_i64().unwrap_or(1);

        let api_key = api_key()?;
        let base = base_url();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let body = serde_json::json!({
            "model": "agnes-image-2.1-flash",
            "prompt": prompt,
            "size": size,
            "n": count,
        });

        let url = format!("{}/v1/images/generations", base);
        let resp = client.post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let img_status = resp.status();
        if !img_status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Ok(ToolOutput::error(format!(
                "image generation failed (HTTP {}): {}",
                img_status, text
            )));
        }

        // Parse response — OpenAI-compatible format or custom
        #[derive(serde::Deserialize)]
        struct ImageResponse {
            data: Option<Vec<ImageData>>,
            images: Option<Vec<ImageData>>,
            url: Option<String>,
            urls: Option<Vec<String>>,
            #[allow(dead_code)]
            error: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct ImageData {
            url: Option<String>,
            image_url: Option<String>,
        }

        let image_resp: ImageResponse = resp.json().await?;

        let mut urls = Vec::new();
        if let Some(data) = image_resp.data {
            for d in data {
                if let Some(u) = d.url.or(d.image_url) {
                    urls.push(u);
                }
            }
        }
        if let Some(images) = image_resp.images {
            for d in images {
                if let Some(u) = d.url.or(d.image_url) {
                    urls.push(u);
                }
            }
        }
        if let Some(u) = image_resp.url {
            urls.push(u);
        }
        if let Some(urls_list) = image_resp.urls {
            urls.extend(urls_list);
        }

        if urls.is_empty() {
            return Ok(ToolOutput::error(
                "Image generation returned no URLs. Check the API response."
            ));
        }

        let result: String = urls.iter().enumerate()
            .map(|(i, u)| format!("{}. {}", i + 1, u))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(ToolOutput::text(format!(
            "✅ Image(s) generated!\n{}\n\nPrompt: {}",
            result, prompt
        )))
    }
}

// ── Registration ───────────────────────────────────────────────────────────

pub fn register_agnes_tools(registry: &mut flint_agent::ToolRegistry) {
    registry.register(VideoGenTool);
    registry.register(ImageGenTool);
}
