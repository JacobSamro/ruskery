//! Tigris (S3-compatible) object storage adapter.
//!
//! Blobs are content-addressable and namespaced per org:
//! `orgs/<org_id>/blobs/<algo>/<hex>`. In-progress uploads land at a temporary
//! key and are server-side-copied to their final content-addressed key once the
//! digest is verified, so layer bytes are never re-uploaded.

use std::time::Duration;

use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_sdk_s3::Client;

use crate::config::StorageConfig;
use crate::error::{Error, Result};

/// Handle to the object store.
#[derive(Clone)]
pub struct Storage {
    client: Client,
    bucket: String,
    presign_ttl: Duration,
}

fn map_s3<E: std::fmt::Display>(ctx: &str, e: E) -> Error {
    Error::Other(anyhow::anyhow!("storage {ctx}: {e}"))
}

impl Storage {
    /// Build the S3 client for the configured Tigris endpoint.
    pub async fn new(cfg: &StorageConfig) -> Result<Self> {
        let creds = Credentials::new(
            cfg.access_key_id.clone(),
            cfg.secret_access_key.clone(),
            None,
            None,
            "ruskery",
        );
        let s3_cfg = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(Region::new(cfg.region.clone()))
            .endpoint_url(&cfg.endpoint)
            .credentials_provider(creds)
            .force_path_style(cfg.force_path_style)
            // Only add/validate flexible checksums when an operation requires
            // them. Keeps wire traffic lean and maximizes compatibility with
            // S3-compatible stores (Tigris, RustFS, MinIO) that may not support
            // the SDK's newer default CRC32 trailers.
            .request_checksum_calculation(
                aws_sdk_s3::config::RequestChecksumCalculation::WhenRequired,
            )
            .response_checksum_validation(
                aws_sdk_s3::config::ResponseChecksumValidation::WhenRequired,
            )
            .build();

        Ok(Self {
            client: Client::from_conf(s3_cfg),
            bucket: cfg.bucket.clone(),
            presign_ttl: Duration::from_secs(cfg.presign_ttl_secs),
        })
    }

    /// Final content-addressed key for a blob (`digest` = `sha256:<hex>`).
    pub fn blob_key(org_id: &str, digest: &str) -> String {
        let (algo, hex) = digest.split_once(':').unwrap_or(("sha256", digest));
        format!("orgs/{org_id}/blobs/{algo}/{hex}")
    }

    /// Temporary key for an in-progress upload.
    pub fn upload_key(org_id: &str, upload_id: &str) -> String {
        format!("orgs/{org_id}/_uploads/{upload_id}")
    }

    // ── blob existence / delivery ──────────────────────────────────

    /// Whether a blob object exists, returning its size. (Storage-level probe;
    /// the hot path uses the DB index instead.)
    #[allow(dead_code)]
    pub async fn head(&self, key: &str) -> Result<Option<i64>> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(out) => Ok(Some(out.content_length().unwrap_or(0))),
            Err(e) => {
                if e.as_service_error()
                    .map(|s| s.is_not_found())
                    .unwrap_or(false)
                {
                    Ok(None)
                } else {
                    Err(map_s3("head", e.into_service_error()))
                }
            }
        }
    }

    /// Presigned GET URL handed to clients on pull (served from Tigris/CDN).
    pub async fn presign_get(&self, key: &str) -> Result<String> {
        let cfg =
            PresigningConfig::expires_in(self.presign_ttl).map_err(|e| map_s3("presign cfg", e))?;
        let req = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(cfg)
            .await
            .map_err(|e| map_s3("presign", e))?;
        Ok(req.uri().to_string())
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| map_s3("delete", e))?;
        Ok(())
    }

    // ── multipart upload ───────────────────────────────────────────

    pub async fn create_multipart(&self, key: &str) -> Result<String> {
        let out = self
            .client
            .create_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| map_s3("create_multipart", e))?;
        out.upload_id()
            .map(|s| s.to_string())
            .ok_or_else(|| map_s3("create_multipart", "missing upload id"))
    }

    /// Upload one part, returning the completed-part descriptor.
    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        part_number: i32,
        body: Vec<u8>,
    ) -> Result<CompletedPart> {
        let out = self
            .client
            .upload_part()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(part_number)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| map_s3("upload_part", e))?;
        Ok(CompletedPart::builder()
            .set_e_tag(out.e_tag().map(|s| s.to_string()))
            .part_number(part_number)
            .build())
    }

    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        parts: Vec<CompletedPart>,
    ) -> Result<()> {
        let completed = CompletedMultipartUpload::builder()
            .set_parts(Some(parts))
            .build();
        self.client
            .complete_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(completed)
            .send()
            .await
            .map_err(|e| map_s3("complete_multipart", e))?;
        Ok(())
    }

    pub async fn abort_multipart(&self, key: &str, upload_id: &str) -> Result<()> {
        self.client
            .abort_multipart_upload()
            .bucket(&self.bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map_err(|e| map_s3("abort_multipart", e))?;
        Ok(())
    }

    /// Server-side copy (used to move a verified upload to its final key).
    pub async fn copy(&self, from_key: &str, to_key: &str) -> Result<()> {
        let source = format!("{}/{}", self.bucket, from_key);
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .key(to_key)
            .copy_source(&source)
            .send()
            .await
            .map_err(|e| map_s3("copy", e))?;
        Ok(())
    }

    /// Store a small object in a single request (used for empty/tiny blobs).
    pub async fn put(&self, key: &str, body: Vec<u8>) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(body))
            .send()
            .await
            .map_err(|e| map_s3("put", e))?;
        Ok(())
    }
}
