use anyhow::Context;
use lapin::{options::BasicPublishOptions, BasicProperties, Channel};
use serde::Serialize;

/// Queue name — must match the worker's consumer (slice 5.1).
pub const INGEST_QUEUE: &str = "ingest_jobs";

/// The job payload the worker will receive.
#[derive(Serialize)]
pub struct IngestJob {
    pub document_id: String,
    pub tenant_id: String,
    pub object_key: String,
    pub filename: String,
}

pub async fn publish_ingest_job(channel: &Channel, job: &IngestJob) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(job)?;
    channel
        .basic_publish(
            "", // default exchange: routing key == queue name
            INGEST_QUEUE,
            BasicPublishOptions::default(),
            &payload,
            // delivery_mode = 2 => persistent message; with a durable queue it survives a broker restart.
            BasicProperties::default().with_delivery_mode(2),
        )
        .await
        .context("failed to publish ingest job")?;
    Ok(())
}
