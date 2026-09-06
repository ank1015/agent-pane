use axum::{Json, Router, extract::Path, http::HeaderMap, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use execution_client::{ExecutionClient, ExecutionClientConfig, GatewayHostRuntime};
use execution_core::*;
use execution_supervisor_core::{
    SupervisorConfig, SupervisorLimits, SupervisorRoot, SupervisorRuntime,
};
use execution_wire::{OperationResult, RequestEnvelope, ResponseEnvelope, dispatch_request};
use image::{GenericImageView, ImageBuffer, ImageFormat, Rgba};
use std::{
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tool_view_image::{
    ViewImageConfig, ViewImageDetail, ViewImageInput, ViewImageOptions, ViewImageTool,
    input_schema, input_schema_with_options, output_schema, output_schema_with_options,
};

struct Host {
    temp: tempfile::TempDir,
    runtime: GatewayHostRuntime,
    calls: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Host {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Host {
    async fn new(content: &[u8], fault: &str) -> Self {
        let temp = tempfile::tempdir().expect("create temp dir");
        let root = temp.path().join("workspace");
        std::fs::create_dir_all(root.join("project/directory")).expect("create workspace");
        std::fs::write(root.join("project/image"), content).expect("write fixture");
        let supervisor = Arc::new(
            SupervisorRuntime::new(SupervisorConfig {
                host_id: ExecutionHostId::generate(),
                state_directory: temp.path().join("state"),
                roots: vec![SupervisorRoot {
                    id: RootId::new("work").expect("root id"),
                    name: "Work".into(),
                    path: root,
                    read_only: false,
                }],
                limits: SupervisorLimits {
                    max_read_bytes: if fault == "small-limit" {
                        3
                    } else {
                        SupervisorLimits::default().max_read_bytes
                    },
                    ..Default::default()
                },
            })
            .await
            .expect("start supervisor"),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let call_count = Arc::clone(&calls);
        let target = Arc::clone(&supervisor);
        let fault = fault.to_owned();
        let app = Router::new().route(
            "/v1/hosts/{host}/operations",
            post(
                move |Path(id): Path<String>,
                      headers: HeaderMap,
                      Json(request): Json<RequestEnvelope>| {
                    let target = Arc::clone(&target);
                    let call_count = Arc::clone(&call_count);
                    let fault = fault.clone();
                    async move {
                        assert_eq!(
                            headers["authorization"],
                            "Bearer view-image-tool-test-token"
                        );
                        assert_eq!(id, target.descriptor().host_id.as_str());
                        call_count.fetch_add(1, Ordering::SeqCst);
                        let mut reply =
                            dispatch_request(target.as_ref(), &OperationContext::new(), request)
                                .await;
                        if let ResponseEnvelope::Success {
                            result: OperationResult::ReadFile(value),
                            ..
                        } = &mut reply
                        {
                            if fault == "revision" && value.offset > 0 {
                                value.metadata.revision =
                                    Some(FileRevision::new("changed").expect("revision"));
                            }
                            if fault == "offset" {
                                value.offset += 1;
                            }
                            if fault == "path" {
                                value.metadata.path.path = "other".into();
                            }
                            if fault == "eof" {
                                value.eof = true;
                            }
                        }
                        Json(reply)
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind server");
        let url = format!("http://{}", listener.local_addr().expect("local address"));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve fixture");
        });
        let mut config =
            ExecutionClientConfig::new(url.parse().expect("URL"), "view-image-tool-test-token");
        config.allow_insecure_http = true;
        let runtime = ExecutionClient::new(config)
            .expect("client")
            .connect_host(
                &OperationContext::new(),
                supervisor.descriptor().host_id.clone(),
            )
            .await
            .expect("connect host");
        Self {
            temp,
            runtime,
            calls,
            server,
        }
    }

    fn tool(&self, config: ViewImageConfig) -> ViewImageTool<'_> {
        ViewImageTool::new(
            &self.runtime,
            ExecutionPath::new(RootId::new("work").expect("root id"), "project").expect("cwd"),
            config,
        )
        .expect("construct tool")
    }
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = ImageBuffer::from_pixel(width, height, Rgba([12u8, 90, 210, 255]));
    let mut output = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut output), ImageFormat::Png)
        .expect("encode fixture");
    output
}

fn input(detail: Option<ViewImageDetail>) -> ViewImageInput {
    ViewImageInput {
        path: "image".into(),
        detail,
        environment_id: None,
    }
}

fn context() -> OperationContext {
    OperationContext::with_timeout(Duration::from_secs(10))
}

fn data_url_bytes(value: &str) -> Vec<u8> {
    let (_, encoded) = value.split_once(',').expect("data URL separator");
    STANDARD.decode(encoded).expect("base64 image")
}

#[tokio::test]
async fn remote_code_mode_preserves_large_original_bytes() {
    let source = png(2304, 864);
    let host = Host::new(&source, "").await;
    let output = host
        .tool(ViewImageConfig {
            chunk_bytes: 127,
            ..Default::default()
        })
        .execute(&context(), input(None))
        .await
        .expect("view image");
    assert_eq!(output.detail, Some(ViewImageDetail::High));
    assert!(
        output
            .image_url
            .starts_with("data:application/octet-stream;base64,")
    );
    let image = image::load_from_memory(&data_url_bytes(&output.image_url)).expect("decode output");
    assert_eq!(image.dimensions(), (2304, 864));
    assert_eq!(data_url_bytes(&output.image_url), source);
    assert_eq!(
        output.code_mode_result()["detail"],
        serde_json::json!("high")
    );
    assert!(output.log_output().starts_with("<image data URL omitted:"));
}

#[tokio::test]
async fn original_preserves_supported_source_bytes() {
    let source = png(40, 20);
    let host = Host::new(&source, "").await;
    let output = host
        .tool(ViewImageConfig {
            options: ViewImageOptions {
                can_request_original_detail: true,
                ..Default::default()
            },
            ..Default::default()
        })
        .execute(&context(), input(Some(ViewImageDetail::Original)))
        .await
        .expect("view original image");
    assert_eq!(output.detail, Some(ViewImageDetail::Original));
    assert_eq!(data_url_bytes(&output.image_url), source);
}

#[tokio::test]
async fn reads_respect_host_limits_and_reject_inconsistent_chunks() {
    let source = png(8, 8);
    let host = Host::new(&source, "small-limit").await;
    host.tool(ViewImageConfig::default())
        .execute(&context(), input(None))
        .await
        .expect("read through small chunks");
    assert!(host.calls.load(Ordering::SeqCst) > source.len() / 3);

    for (fault, code) in [
        ("revision", ExecutionErrorCode::RevisionConflict),
        ("offset", ExecutionErrorCode::Internal),
        ("path", ExecutionErrorCode::Internal),
        ("eof", ExecutionErrorCode::RevisionConflict),
    ] {
        let host = Host::new(&source, fault).await;
        let failure = host
            .tool(ViewImageConfig {
                chunk_bytes: 3,
                ..Default::default()
            })
            .execute(&context(), input(None))
            .await
            .expect_err("malformed read must fail");
        assert_eq!(failure.code, code, "{fault}");
    }
}

#[tokio::test]
async fn invalid_missing_directory_and_oversize_inputs_are_rejected() {
    let host = Host::new(b"not an image", "").await;
    let failure = host
        .tool(ViewImageConfig::default())
        .execute(&context(), input(None))
        .await
        .expect_err("invalid image must fail");
    assert_eq!(failure.code, ExecutionErrorCode::Unsupported);
    assert_eq!(failure.details["source"], "tool-view-image");

    let tool = host.tool(ViewImageConfig::default());
    for (path, code) in [
        ("missing", ExecutionErrorCode::NotFound),
        ("directory", ExecutionErrorCode::IsDirectory),
    ] {
        let failure = tool
            .execute(
                &context(),
                ViewImageInput {
                    path: path.into(),
                    detail: None,
                    environment_id: None,
                },
            )
            .await
            .expect_err("invalid path target must fail");
        assert_eq!(failure.code, code);
    }

    let failure = host
        .tool(ViewImageConfig {
            max_image_bytes: 4,
            ..Default::default()
        })
        .execute(&context(), input(None))
        .await
        .expect_err("oversize image must fail");
    assert_eq!(failure.code, ExecutionErrorCode::ResourceExhausted);
}

#[tokio::test]
async fn paths_and_cancellation_fail_before_file_reads() {
    let host = Host::new(&png(2, 2), "").await;
    let tool = host.tool(ViewImageConfig::default());
    for path in ["../../image", "/outside/image"] {
        let before = host.calls.load(Ordering::SeqCst);
        let failure = tool
            .execute(
                &context(),
                ViewImageInput {
                    path: path.into(),
                    detail: None,
                    environment_id: None,
                },
            )
            .await
            .expect_err("path escape must fail");
        assert!(matches!(
            failure.code,
            ExecutionErrorCode::InvalidPath | ExecutionErrorCode::PathOutsideRoot
        ));
        assert_eq!(host.calls.load(Ordering::SeqCst), before);
    }

    let cancelled = context();
    cancelled.cancel();
    let before = host.calls.load(Ordering::SeqCst);
    let failure = tool
        .execute(&cancelled, input(None))
        .await
        .expect_err("cancelled operation must fail");
    assert_eq!(failure.code, ExecutionErrorCode::Cancelled);
    assert_eq!(host.calls.load(Ordering::SeqCst), before);

    let native_root = &host.runtime.descriptor().roots[0].native_path;
    let output = tool
        .execute(
            &context(),
            ViewImageInput {
                path: format!("{native_root}/project/image"),
                detail: None,
                environment_id: None,
            },
        )
        .await
        .expect("absolute path in root");
    assert_eq!(output.detail, Some(ViewImageDetail::High));
    assert!(host.temp.path().exists());
}

#[test]
fn schemas_and_serde_match_the_codex_contract() {
    let capable = ViewImageOptions {
        can_request_original_detail: true,
        ..Default::default()
    };
    let inputs = jsonschema::validator_for(&input_schema_with_options(capable)).unwrap();
    for value in [
        serde_json::json!({"path":""}),
        serde_json::json!({"path":"image", "detail":"original"}),
    ] {
        assert!(inputs.is_valid(&value));
    }
    let default_inputs = jsonschema::validator_for(&input_schema()).unwrap();
    assert!(!default_inputs.is_valid(&serde_json::json!({"path":"image", "detail":"original"})));
    // Runtime parsing deliberately accepts hidden legacy hints and unknown keys.
    let parsed: ViewImageInput = serde_json::from_value(
        serde_json::json!({"path":"image", "detail":"original", "extra":true}),
    )
    .unwrap();
    assert_eq!(parsed.detail, Some(ViewImageDetail::Original));
    let failure = serde_json::from_value::<ViewImageInput>(
        serde_json::json!({"path":"image", "detail":"low"}),
    )
    .unwrap_err();
    assert_eq!(
        failure.to_string(),
        "view_image.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `low`"
    );
    let outputs = jsonschema::validator_for(&output_schema()).unwrap();
    assert!(outputs.is_valid(&serde_json::json!({"image_url":"url", "detail":"high"})));
    assert!(!outputs.is_valid(&serde_json::json!({"image_url":"url"})));
    let unified = ViewImageOptions {
        unified_image_budget: true,
        ..capable
    };
    assert!(
        input_schema_with_options(unified)["properties"]
            .get("detail")
            .is_none()
    );
    let outputs = jsonschema::validator_for(&output_schema_with_options(unified)).unwrap();
    assert!(outputs.is_valid(&serde_json::json!({"image_url":"url"})));
    assert!(!outputs.is_valid(&serde_json::json!({"image_url":"url", "detail":"original"})));
}

fn inline_image(
    content: &llm_contracts::ContentPart,
) -> (Vec<u8>, &str, Option<llm_contracts::ImageDetail>) {
    let llm_contracts::ContentPart::Image(image) = content else {
        panic!("expected actual image content")
    };
    let llm_contracts::ImageSource::Base64(source) = &image.source else {
        panic!("expected inline content")
    };
    (
        STANDARD.decode(&source.data).unwrap(),
        &source.mime_type,
        image.detail,
    )
}

#[tokio::test]
async fn capabilities_control_schemas_raw_hints_and_prompt_preparation() {
    use llm_contracts::{ImageDetail, ToolArguments};
    let source = png(2304, 864);
    let host = Host::new(&source, "").await;
    for supports_original in [false, true] {
        for unified in [false, true] {
            for requested in [
                None,
                Some(ViewImageDetail::High),
                Some(ViewImageDetail::Original),
            ] {
                let options = ViewImageOptions {
                    supports_images: true,
                    can_request_original_detail: supports_original,
                    unified_image_budget: unified,
                };
                let tool = host.tool(ViewImageConfig {
                    options,
                    ..Default::default()
                });
                assert_eq!(
                    tool.input_schema()["properties"].get("detail").is_some(),
                    supports_original && !unified
                );
                let raw = tool.execute(&context(), input(requested)).await.unwrap();
                assert_eq!(data_url_bytes(&raw.image_url), source);
                let original =
                    unified || (supports_original && requested == Some(ViewImageDetail::Original));
                let expected = if original {
                    ViewImageDetail::Original
                } else {
                    ViewImageDetail::High
                };
                assert_eq!(raw.detail, (!unified).then_some(expected));
                assert_eq!(raw.code_mode_result().get("detail").is_none(), unified);
                let content = tool
                    .execute_for_model(&context(), input(requested))
                    .await
                    .unwrap();
                let (bytes, mime, detail) = inline_image(&content);
                assert_eq!(mime, "image/png");
                assert_eq!(
                    image::load_from_memory(&bytes).unwrap().dimensions(),
                    if original { (2304, 864) } else { (2048, 768) }
                );
                assert_eq!(
                    detail,
                    Some(if original {
                        ImageDetail::Original
                    } else {
                        ImageDetail::High
                    })
                );
            }
        }
    }
    let disabled = host.tool(ViewImageConfig {
        options: ViewImageOptions {
            supports_images: false,
            ..Default::default()
        },
        ..Default::default()
    });
    let before = host.calls.load(Ordering::SeqCst);
    for failure in [
        disabled.execute(&context(), input(None)).await.unwrap_err(),
        disabled
            .execute_for_model(&context(), input(None))
            .await
            .unwrap_err(),
        disabled
            .parse_arguments(&ToolArguments::String("invalid".into()))
            .unwrap_err(),
    ] {
        assert_eq!(
            failure.message,
            "view_image is not allowed because you do not support image inputs"
        );
    }
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
    assert_eq!(
        std::fs::read(host.temp.path().join("workspace/project/image")).unwrap(),
        source
    );
}

#[tokio::test]
async fn gif_bytes_remain_original_for_code_mode_and_convert_for_model_history() {
    let mut source = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut source);
        for rgba in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(image::Frame::new(ImageBuffer::from_pixel(4, 4, Rgba(rgba))))
                .unwrap();
        }
    }
    let host = Host::new(&source, "").await;
    let tool = host.tool(ViewImageConfig::default());
    let raw = tool.execute(&context(), input(None)).await.unwrap();
    assert_eq!(data_url_bytes(&raw.image_url), source);
    let content = tool
        .execute_for_model(&context(), input(None))
        .await
        .unwrap();
    let (bytes, mime, _) = inline_image(&content);
    assert_eq!(mime, "image/png");
    assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Png);
    assert_eq!(
        image::load_from_memory(&bytes)
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
            .0,
        [255, 0, 0, 255]
    );
}

#[tokio::test]
async fn normalized_paths_and_legacy_arguments_resolve_on_the_selected_host() {
    let source = png(8, 8);
    let host = Host::new(&source, "").await;
    let tool = host.tool(ViewImageConfig::default());
    for path in ["missing/../image", "../project/image"] {
        let output = tool
            .execute(
                &context(),
                ViewImageInput {
                    path: path.into(),
                    ..input(None)
                },
            )
            .await
            .unwrap();
        assert_eq!(data_url_bytes(&output.image_url), source);
    }
    let parsed = tool
        .parse_arguments(&llm_contracts::ToolArguments::Object(
            serde_json::from_value(
                serde_json::json!({"path":"image", "detail":"original", "extra":true}),
            )
            .unwrap(),
        ))
        .unwrap();
    assert_eq!(
        tool.execute(&context(), parsed).await.unwrap().detail,
        Some(ViewImageDetail::High)
    );
    let before = host.calls.load(Ordering::SeqCst);
    assert!(
        tool.execute(
            &context(),
            ViewImageInput {
                environment_id: Some("unselected".into()),
                ..input(None)
            }
        )
        .await
        .is_err()
    );
    assert_eq!(host.calls.load(Ordering::SeqCst), before);
}

// An HTTP object store exercises the same publisher boundary a deployment's
// bucket adapter implements. No image bytes are carried in delivered history.
struct HttpPublisher {
    base_url: String,
    client: reqwest::Client,
}
impl tool_view_image::ImagePublisher for HttpPublisher {
    fn publish<'a>(
        &'a self,
        context: &'a OperationContext,
        asset: tool_view_image::ImageAsset<'a>,
    ) -> tool_view_image::PublishImageFuture<'a> {
        Box::pin(async move {
            context.checkpoint()?;
            let url = format!("{}/images/{}", self.base_url, asset.sha256);
            self.client
                .put(&url)
                .header("content-type", asset.mime_type)
                .body(asset.bytes.to_vec())
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            context.checkpoint()?;
            Ok(url)
        })
    }
}

#[tokio::test]
async fn hosted_delivery_returns_small_replayable_urls_for_original_and_prepared_images() {
    use axum::{
        body::Bytes,
        routing::{get, put},
    };
    use llm_contracts::{ContentPart, ImageSource, Validate as _};
    use std::collections::HashMap;
    use std::sync::Mutex;
    let objects = Arc::new(Mutex::new(HashMap::<String, (String, Vec<u8>)>::new()));
    let writes = objects.clone();
    let reads = objects.clone();
    let app = Router::new().route(
        "/images/{key}",
        put(
            move |Path(key): Path<String>, headers: HeaderMap, body: Bytes| {
                let writes = writes.clone();
                async move {
                    writes.lock().unwrap().insert(
                        key,
                        (
                            headers["content-type"].to_str().unwrap().into(),
                            body.to_vec(),
                        ),
                    );
                }
            },
        )
        .merge(get(move |Path(key): Path<String>| {
            let reads = reads.clone();
            async move {
                let (mime, bytes) = reads.lock().unwrap().get(&key).unwrap().clone();
                ([("content-type", mime)], bytes)
            }
        })),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let source = png(2304, 864);
    let host = Host::new(&source, "").await;
    let tool = host.tool(ViewImageConfig {
        delivery: tool_view_image::ImageDelivery::Hosted(Arc::new(HttpPublisher {
            base_url,
            client: reqwest::Client::new(),
        })),
        ..Default::default()
    });
    let raw = tool.execute(&context(), input(None)).await.unwrap();
    assert!(raw.image_url.starts_with("http://"));
    assert!(serde_json::to_vec(&raw).unwrap().len() < 250);
    let fetched = reqwest::get(&raw.image_url).await.unwrap();
    assert_eq!(fetched.headers()["content-type"], "image/png");
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), source);
    let content = tool
        .execute_for_model(&context(), input(None))
        .await
        .unwrap();
    content.validate().unwrap();
    let ContentPart::Image(image) = &content else {
        panic!("image")
    };
    let ImageSource::Url(url) = &image.source else {
        panic!("hosted URL")
    };
    let body = provider_body(content.clone(), tool.definition(), false);
    assert_eq!(body["input"][0]["type"], "function_call_output");
    assert_eq!(body["input"][0]["output"][0]["type"], "input_image");
    assert_eq!(body["input"][0]["output"][0]["image_url"], url.url);
    assert!(!body.to_string().contains("base64"));
    let prepared = reqwest::get(&url.url).await.unwrap().bytes().await.unwrap();
    assert_eq!(
        image::load_from_memory(&prepared).unwrap().dimensions(),
        (2048, 768)
    );
    assert_ne!(url.url, raw.image_url);
    let checkpoint = serde_json::to_vec(&content).unwrap();
    assert!(checkpoint.len() < 350);
    assert!(!String::from_utf8_lossy(&checkpoint).contains("base64"));
    let restored: ContentPart = serde_json::from_slice(&checkpoint).unwrap();
    assert_eq!(restored, content);
    assert_eq!(
        tool.execute_for_model(&context(), input(None))
            .await
            .unwrap(),
        content
    );
    assert_eq!(objects.lock().unwrap().len(), 2); // stable content keys on repeat
    assert_eq!(
        std::fs::read(host.temp.path().join("workspace/project/image")).unwrap(),
        source
    );
    server.abort();
}

struct BadPublisher {
    url: Option<&'static str>,
    calls: AtomicUsize,
}
impl tool_view_image::ImagePublisher for BadPublisher {
    fn publish<'a>(
        &'a self,
        _: &'a OperationContext,
        _: tool_view_image::ImageAsset<'a>,
    ) -> tool_view_image::PublishImageFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.url.map(str::to_owned).ok_or_else(|| {
                ExecutionError::new(ExecutionErrorCode::Unavailable, "publication failed")
            })
        })
    }
}

#[tokio::test]
async fn hosted_errors_do_not_fall_back_to_inline_and_invalid_images_are_never_published() {
    for url in [
        None,
        Some("data:image/png;base64,secret"),
        Some("file:///secret"),
    ] {
        let publisher = Arc::new(BadPublisher {
            url,
            calls: AtomicUsize::new(0),
        });
        let host = Host::new(&png(8, 8), "").await;
        let tool = host.tool(ViewImageConfig {
            delivery: tool_view_image::ImageDelivery::Hosted(publisher.clone()),
            ..Default::default()
        });
        let failure = tool.execute(&context(), input(None)).await.unwrap_err();
        assert_eq!(
            failure.code,
            if url.is_some() {
                ExecutionErrorCode::InvalidRequest
            } else {
                ExecutionErrorCode::Unavailable
            }
        );
        assert!(!failure.message.contains("secret"));
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
        let cancelled = context();
        cancelled.cancel();
        assert!(tool.execute(&cancelled, input(None)).await.is_err());
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    }
    let publisher = Arc::new(BadPublisher {
        url: Some("https://example.test/private?signature=secret"),
        calls: AtomicUsize::new(0),
    });
    let host = Host::new(b"not an image", "").await;
    assert!(
        host.tool(ViewImageConfig {
            delivery: tool_view_image::ImageDelivery::Hosted(publisher.clone()),
            ..Default::default()
        })
        .execute(&context(), input(None))
        .await
        .is_err()
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    let output = tool_view_image::ViewImageOutput {
        image_url: publisher.url.unwrap().into(),
        detail: Some(ViewImageDetail::High),
    };
    assert!(!format!("{output:?}").contains("secret"));
    assert!(!output.log_output().contains("signature"));
}

fn provider_body(
    content: llm_contracts::ContentPart,
    definition: llm_contracts::ToolDefinition,
    lite: bool,
) -> serde_json::Value {
    use llm_contracts::*;
    let request = LlmRequest {
        model: ModelRef {
            provider: ProviderId::new("openai").unwrap(),
            id: ModelId::new("gpt-5.6-sol").unwrap(),
            name: None,
        },
        instructions: None,
        metadata: Default::default(),
        provider_options: serde_json::Map::from_iter([(
            provider_openai::CODEX_RESPONSES_LITE_OPTION.into(),
            serde_json::json!(lite),
        )]),
        tools: vec![definition],
        messages: vec![Message::ToolResult(ToolResultMessage {
            id: MessageId::new("result-1").unwrap(),
            tool_name: "view_image".into(),
            tool_call_id: ToolCallId::new("call-1").unwrap(),
            content: vec![content],
            details: None,
            timestamp: Timestamp(1),
            outcome: ToolResultOutcome::Success,
        })],
    };
    provider_openai::build_response_request(&request).unwrap()
}

#[tokio::test]
async fn inline_tool_images_reach_provider_as_images_with_original_detail_or_lite_omission() {
    let host = Host::new(&png(8, 8), "").await;
    let tool = host.tool(ViewImageConfig {
        options: ViewImageOptions {
            can_request_original_detail: true,
            ..Default::default()
        },
        ..Default::default()
    });
    let content = tool
        .execute_for_model(&context(), input(Some(ViewImageDetail::Original)))
        .await
        .unwrap();
    for lite in [false, true] {
        let body = provider_body(content.clone(), tool.definition(), lite);
        let result = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        let image = &result["output"][0];
        assert_eq!(image["type"], "input_image");
        assert!(
            image["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        if lite {
            assert!(image.get("detail").is_none());
        } else {
            assert_eq!(image["detail"], "original");
        }
    }
}
