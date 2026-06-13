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

// ── VideoGen Tool ──────────────────────────────────────────────────────────

/// Async video generation tool.
///
/// Workflow:
/// 1. POST /v1/videos → returns task_id
/// 2. GET /v1/videos/{task_id} → polls until done, returns video URL
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
            description: "Generate a video from a text prompt or input image using \
                the Agnes AI video generation model (agnes-video-v2.0). \
                This is an async operation: submits the task, polls for completion, \
                and returns the video URL. \
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

        // Parse response for task_id
        #[derive(serde::Deserialize)]
        struct SubmitResponse {
            task_id: Option<String>,
            id: Option<String>,
            #[allow(dead_code)]
            error: Option<serde_json::Value>,
        }
        let submit: SubmitResponse = resp.json().await?;
        let task_id = submit.task_id.or(submit.id).ok_or_else(|| {
            anyhow::anyhow!("video generation submitted but no task_id returned")
        })?;

        // Step 2: Poll for completion
        let retrieve_url = format!("{}/v1/videos/{}", base, task_id);
        let mut attempts = 0u32;
        let max_attempts = 120; // 10 minutes with 5s interval
        let poll_interval = std::time::Duration::from_secs(5);

        loop {
            if attempts >= max_attempts {
                return Ok(ToolOutput::text(format!(
                    "Video generation task {} is still processing. \
                     Use task_id {} to check later at {}/v1/videos/{}.",
                    task_id, task_id, base, task_id
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
                    "failed to poll task {}: HTTP {} {}",
                    task_id, poll_status, text
                )));
            }

            #[derive(serde::Deserialize)]
            struct TaskResponse {
                status: Option<String>,
                progress: Option<f64>,
                output: Option<serde_json::Value>,
                video_url: Option<String>,
                url: Option<String>,
                remixed_from_video_id: Option<String>,
                video_id: Option<String>,
                error: Option<String>,
                #[allow(dead_code)]
                task_id: Option<String>,
            }
            let task: TaskResponse = resp.json().await?;

            // Check for completion by status or progress
            let is_completed = matches!(task.status.as_deref(), Some("completed"))
                || task.progress.map(|p| p >= 100.0).unwrap_or(false);
            let is_failed = matches!(task.status.as_deref(), Some("failed") | Some("error"));

            if is_completed {
                // Extract video URL from various possible fields
                let video_url = task.remixed_from_video_id
                    .or(task.video_url)
                    .or(task.url)
                    .or_else(|| task.output.as_ref().and_then(|o| o["url"].as_str().map(String::from)))
                    .or_else(|| task.output.as_ref().and_then(|o| o["video_url"].as_str().map(String::from)))
                    .unwrap_or_else(|| task.video_id.clone().unwrap_or_else(|| "(completed, check UI)".to_string()));

                return Ok(ToolOutput::text(format!(
                    "✅ Video generation complete!\n\
                     Task ID: {}\n\
                     Video URL: {}\n\
                     Prompt: {}",
                    task_id, video_url, prompt
                )));
            }

            if is_failed {
                let err_msg = task.error.unwrap_or_else(|| "unknown error".to_string());
                return Ok(ToolOutput::error(format!(
                    "Video generation failed: {}", err_msg
                )));
            }

            // Still running (queued, processing, pending, or low progress)
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
