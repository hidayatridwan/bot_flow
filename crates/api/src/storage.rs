use anyhow::Context;
use s3::{creds::Credentials, Bucket, BucketConfiguration, Region};

use crate::config::Config;

fn region(cfg: &Config) -> Region {
    // MinIO ignores the region name but rust-s3 requires one; the endpoint is what matters.
    Region::Custom {
        region: cfg.s3_region.clone(),
        endpoint: cfg.s3_endpoint.clone(),
    }
}

fn creds(cfg: &Config) -> anyhow::Result<Credentials> {
    Credentials::new(
        Some(&cfg.s3_access_key),
        Some(&cfg.s3_secret_key),
        None,
        None,
        None,
    )
    .context("invalid S3 credentials")
}

/// A bucket handle. `with_path_style` is required for MinIO (bucket in the URL path,
/// not as a subdomain like real AWS).
pub fn build_bucket(cfg: &Config) -> anyhow::Result<Box<Bucket>> {
    Ok(Bucket::new(&cfg.s3_bucket, region(cfg), creds(cfg)?)?.with_path_style())
}

/// A bucket handle bound to the endpoint **the client will connect to**, used only to sign
/// presigned URLs.
///
/// SigV4 covers the `Host` header. Sign with the internal endpoint (`http://minio:9000`) while
/// the client PUTs to `https://storage.example.com` and MinIO answers `SignatureDoesNotMatch`.
/// In local dev the two endpoints are identical, so the bug only appears in production.
pub fn build_public_bucket(cfg: &Config) -> anyhow::Result<Box<Bucket>> {
    let region = Region::Custom {
        region: cfg.s3_region.clone(),
        endpoint: cfg.s3_public_endpoint.clone(),
    };
    Ok(Bucket::new(&cfg.s3_bucket, region, creds(cfg)?)?.with_path_style())
}

/// Create the bucket if missing. Safe to call on every startup.
pub async fn ensure_bucket(cfg: &Config) -> anyhow::Result<()> {
    if build_bucket(cfg)?.exists().await? {
        tracing::info!("bucket '{}' already exists", cfg.s3_bucket);
        return Ok(());
    }
    Bucket::create_with_path_style(
        &cfg.s3_bucket,
        region(cfg),
        creds(cfg)?,
        BucketConfiguration::default(),
    )
    .await
    .context("failed to create bucket")?;
    tracing::info!("bucket '{}' created", cfg.s3_bucket);
    Ok(())
}
