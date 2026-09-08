//! Shared aggregation over canonical history; missing usage is not zero.
use super::error::Result;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;
pub(super) async fn read(db: &mut PgConnection, session: Uuid, run: Option<Uuid>) -> Result<Value> {
    // Aggregate all canonical assistant messages, not just one UI history page.
    let mut fields = vec![
        "count(*) as assistant_messages".to_string(),
        "count(*) filter(where jsonb_typeof(message->'usage')='object') as messages_with_usage"
            .to_string(),
    ];
    for (name, path) in [
        ("cost_usd", "{usage,cost,total}"),
        ("input_tokens", "{usage,input}"),
        ("output_tokens", "{usage,output}"),
        ("cache_read_tokens", "{usage,cache_read}"),
        ("cache_write_tokens", "{usage,cache_write}"),
    ] {
        // CASE guards the cast even if PostgreSQL reorders predicates.
        let numeric = format!(
            "case when jsonb_typeof(message#>'{path}')='number' then (message#>>'{path}')::numeric end"
        );
        let valid = format!("case when ({numeric})>=0 then ({numeric}) end");
        fields.push(format!("sum({valid}) as {name}"));
        fields.push(format!("count({valid}) as {name}_messages"));
    }
    let sql = format!(
        "select to_jsonb(t) from (select {} from session_messages sm join messages m on m.id=sm.message_id where sm.session_id=$1 and ($2::uuid is null or sm.run_id=$2) and m.message->>'role'='assistant') t",
        fields.join(",")
    );
    let mut metrics: Value = sqlx::query_scalar(&sql)
        .bind(session)
        .bind(run)
        .fetch_one(&mut *db)
        .await?;
    let elapsed:Option<f64>=sqlx::query_scalar("select sum(extract(epoch from (coalesce(finished_at,clock_timestamp())-started_at)))::double precision from runs where session_id=$1 and ($2::uuid is null or id=$2)").bind(session).bind(run).fetch_one(&mut *db).await?;
    metrics["sessionId"] = json!(session);
    metrics["runId"] = json!(run);
    metrics["runWallSeconds"] = json!(elapsed);
    metrics["scope"] = json!(if run.is_some() {
        "run"
    } else {
        "session_history"
    });
    metrics["completeUsage"] = json!(
        metrics["assistant_messages"] != 0
            && [
                "cost_usd_messages",
                "input_tokens_messages",
                "output_tokens_messages",
                "cache_read_tokens_messages",
                "cache_write_tokens_messages"
            ]
            .iter()
            .all(|k| metrics[*k] == metrics["assistant_messages"])
    );
    let rate = metrics["input_tokens"]
        .as_f64()
        .zip(metrics["cache_read_tokens"].as_f64())
        .and_then(|(i, c)| if i + c > 0.0 { Some(c / (i + c)) } else { None });
    metrics["cacheHitRate"] = json!(rate);
    Ok(metrics)
}
