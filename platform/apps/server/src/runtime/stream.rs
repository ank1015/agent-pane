use super::{
    RuntimeService,
    error::{Result, RuntimeError},
    http,
    model::{SequenceQuery, after},
};
use axum::{
    extract::{
        Path, Query, State,
        rejection::{PathRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use futures_util::stream;
use serde_json::{Value, json};
use std::{collections::VecDeque, convert::Infallible, time::Duration};
use tokio::sync::broadcast;
use uuid::Uuid;

struct Feed {
    service: RuntimeService,
    receiver: broadcast::Receiver<Uuid>,
    run: Uuid,
    cursor: i64,
    buffer: VecDeque<Value>,
    done: bool,
}
impl Feed {
    async fn next(&mut self) -> Result<Option<Event>> {
        loop {
            if let Some(value) = self.buffer.pop_front() {
                let sequence = value["sequence"].as_i64().ok_or(RuntimeError::StoredData)?;
                let kind = value["type"].as_str().ok_or(RuntimeError::StoredData)?;
                if kind.contains(['\r', '\n']) {
                    return Err(RuntimeError::StoredData);
                }
                let event = Event::default()
                    .id(sequence.to_string())
                    .event(kind)
                    .json_data(value)
                    .map_err(|_| RuntimeError::StoredData)?;
                self.cursor = sequence;
                return Ok(Some(event));
            }
            self.buffer = self
                .service
                .event_batch(self.run, self.cursor, 200)
                .await?
                .into();
            if !self.buffer.is_empty() {
                continue;
            }
            let (status, last): (String, i64) =
                sqlx::query_as("select status,last_event_sequence from runs where id=$1")
                    .bind(self.run)
                    .fetch_one(&self.service.pool)
                    .await?;
            if matches!(status.as_str(), "completed" | "failed" | "aborted") && self.cursor >= last
            {
                return Ok(None);
            }
            if last > self.cursor {
                continue;
            }
            // Hints lower latency within this process. Periodic reads also see
            // commits from other Platform replicas and future worker services.
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    match self.receiver.recv().await {
                        Ok(id) if id == self.run => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
            })
            .await;
        }
    }
}

pub(super) async fn events(
    State(service): State<RuntimeService>,
    path: std::result::Result<Path<Uuid>, PathRejection>,
    params: std::result::Result<Query<SequenceQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response> {
    let run = http::id(path)?;
    let q = http::query(params)?;
    if q.status.is_some() || q.limit.is_some() {
        return Err(RuntimeError::Invalid(
            "Streams support after_sequence only, not limit or status filtering.",
        ));
    }
    let mut cursor = after(q.after_sequence)?;
    if let Some(value) = headers.get("last-event-id") {
        cursor = value
            .to_str()
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n >= 0)
            .ok_or(RuntimeError::Invalid(
                "Last-Event-ID must be a nonnegative event sequence.",
            ))?;
    }
    service.exists("runs", "id", run).await?;
    let receiver = service.signals.subscribe();
    let feed = Feed {
        service,
        receiver,
        run,
        cursor,
        buffer: VecDeque::new(),
        done: false,
    };
    let stream = stream::unfold(feed, |mut feed| async move {
        if feed.done {
            return None;
        }
        match feed.next().await {
            Ok(Some(event)) => Some((Ok::<_, Infallible>(event), feed)),
            Ok(None) => None,
            Err(_) => {
                feed.done = true;
                let event=Event::default().event("error").data(json!({"error":{"code":"EVENT_STREAM_UNAVAILABLE","message":"Reconnect using the last received event ID."}}).to_string());
                Some((Ok(event), feed))
            }
        }
    });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    Ok(response)
}
