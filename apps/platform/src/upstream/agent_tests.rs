use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use tokio::{net::TcpListener, sync::mpsc};

use super::AgentClient;

#[derive(Deserialize)]
struct ListQuery {
    limit: u32,
    cursor: Option<String>,
}

#[tokio::test]
async fn harness_list_authenticates_and_forwards_pagination() {
    let (request_tx, mut request_rx) = mpsc::unbounded_channel();
    let mock_agent = Router::new()
        .route("/v1/harnesses", get(capture_harness_list))
        .with_state(request_tx);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_agent).await.unwrap();
    });

    let client = AgentClient::new(
        format!("http://{address}").parse().unwrap(),
        "platform-agent-control-token",
        Duration::from_secs(1),
    )
    .unwrap();
    let page = client.list_harnesses(Some("next-page")).await.unwrap();
    let (authorization, limit, cursor) = request_rx.recv().await.unwrap();

    assert_eq!(authorization, "Bearer platform-agent-control-token");
    assert_eq!(limit, 100);
    assert_eq!(cursor.as_deref(), Some("next-page"));
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].harness_id, "pi");
    assert_eq!(page.items[0].display_name, "Pi");
    assert_eq!(page.next_cursor.as_deref(), Some("another-page"));
}

async fn capture_harness_list(
    State(request_tx): State<mpsc::UnboundedSender<(String, u32, Option<String>)>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Json<serde_json::Value> {
    request_tx
        .send((
            headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned(),
            query.limit,
            query.cursor,
        ))
        .unwrap();
    Json(json!({
        "items": [{
            "harness_id": "pi",
            "slug": "pi",
            "display_name": "Pi",
            "description": "Pi coding agent",
            "enabled": true,
            "active_revision_id": "pi-v1",
            "created_at": "2026-08-27T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z"
        }],
        "next_cursor": "another-page"
    }))
}
