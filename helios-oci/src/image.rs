use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use bollard::query_parameters::{
    CreateImageOptions, ListImagesOptions, RemoveImageOptions, TagImageOptions,
};
use serde::{Deserialize, Serialize};
use tokio_stream::{Stream, StreamExt};

use super::util::types::ImageUri;
use super::{Client, Credentials, Error, Result, WithContext};

use bollard::secret::{CreateImageInfo, ImageInspect};

#[derive(Debug, Clone)]
pub struct Image<'a>(&'a Client);

impl<'a> Image<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self(client)
    }
}

impl Image<'_> {
    /// Returns the list of images on the server.
    pub async fn list(&self) -> Result<Vec<ImageUri>> {
        let opts = ListImagesOptions {
            all: true,
            ..Default::default()
        };

        let res = self.0.inner().list_images(Some(opts)).await;
        let images = res.map_err(Error::with_context("failed to list images"))?;

        let uris = images
            .into_iter()
            .flat_map(|image| {
                image
                    .repo_digests
                    .into_iter()
                    .chain(image.repo_tags)
                    // ignore errors when parsing as the list may contain
                    // <none>:<none> tags
                    .flat_map(|tag| tag.parse().ok())
            })
            // Deduplicate digests and tags ImageUri::repo_and_tag
            // When repo and tag match, prefer the one with a digest.
            .fold(
                HashMap::<String, ImageUri>::new(),
                |mut acc, uri: ImageUri| {
                    let repo_and_tag = uri.repo_and_tag();
                    let exists_with_digest = acc
                        .get(&repo_and_tag)
                        .is_some_and(|existing| existing.digest().is_some());
                    if !exists_with_digest {
                        acc.insert(repo_and_tag, uri);
                    }
                    acc
                },
            )
            .into_values()
            .collect();

        Ok(uris)
    }

    /// Tags an image so that it becomes part of a repository.
    pub async fn tag(&self, name: &str, new_name: &ImageUri) -> Result<()> {
        let repo = new_name.repo();
        let tag = new_name.tag().cloned();
        let opts = TagImageOptions {
            repo: Some(repo),
            tag,
        };

        let res = self.0.inner().tag_image(name, Some(opts)).await;
        res.map_err(Error::from)
            .with_context(|| format!("failed to tag image {name} as {}", new_name.as_str()))?;

        Ok(())
    }

    /// Pulls an image from a registry.
    pub async fn pull(&self, image: &ImageUri, creds: Option<Credentials>) -> Result<()> {
        let mut stream = self.pull_with_progress(image, creds);
        while let Some(result) = stream.next().await {
            result?;
        }
        Ok(())
    }

    /// Pulls an image from a registry, returning a stream of progress updates (current, total).
    pub fn pull_with_progress(&self, image: &ImageUri, creds: Option<Credentials>) -> PullProgress {
        let opts = Some(CreateImageOptions {
            from_image: Some(image.clone().into()),
            ..Default::default()
        });

        PullProgress {
            inner: Box::pin(self.0.inner().create_image(opts, None, creds)),
            image: image.as_str().to_owned(),
        }
    }
}

pub struct PullProgress {
    inner: Pin<
        Box<dyn Stream<Item = std::result::Result<CreateImageInfo, bollard::errors::Error>> + Send>,
    >,
    image: String,
}

impl Stream for PullProgress {
    type Item = Result<(i64, i64)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match self.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => {
                    let err =
                        Error::from(e).context(format!("failed to pull image {}", self.image));
                    return Poll::Ready(Some(Err(err)));
                }
                Poll::Ready(Some(Ok(info))) => {
                    if let Some(detail) = info.progress_detail
                        && let (Some(current), Some(total)) = (detail.current, detail.total)
                    {
                        return Poll::Ready(Some(Ok((current, total))));
                    }
                }
            }
        }
    }
}

impl Image<'_> {
    /// Returns low-level information about an image.
    pub async fn inspect(&self, image: &str) -> Result<LocalImage> {
        let res = self.0.inner().inspect_image(image).await;
        let info = res
            .map_err(Error::from)
            .with_context(|| format!("failed to inspect image {image}"))?;

        info.try_into()
            .with_context(|| format!("failed to inspect image {image}"))
    }

    /// Removes an image, along with any untagged parent images that were referenced by that image.
    pub async fn remove(&self, image: &ImageUri) -> Result<()> {
        self.0
            .inner()
            .remove_image(image.as_str(), Option::<RemoveImageOptions>::None, None)
            .await
            .map_err(Error::from)
            .with_context(|| format!("failed to remove image {}", image.as_str()))?;

        Ok(())
    }
}

#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ImageConfig {
    /// Command to run specified as an array of strings
    pub cmd: Option<Vec<String>>,

    /// User-defined key/value metadata
    pub labels: Option<HashMap<String, String>>,
}

impl From<bollard::config::ImageConfig> for ImageConfig {
    fn from(value: bollard::config::ImageConfig) -> Self {
        let bollard::config::ImageConfig { cmd, labels, .. } = value;
        Self { cmd, labels }
    }
}

#[derive(Debug, Clone)]
pub struct LocalImage {
    /// The content-addressable ID of an image.
    ///
    /// This identifier is a content-addressable digest calculated from the
    /// image's configuration (which includes the digests of layers used by
    /// the image).
    ///
    ///  Note that this digest differs from the `RepoDigests`, which holds
    /// digests of image manifests that reference the image.
    pub id: String,

    /// Configuration of the image. These fields are used as defaults when starting a container from the image.
    pub config: ImageConfig,
}

impl TryFrom<ImageInspect> for LocalImage {
    type Error = Error;

    fn try_from(value: ImageInspect) -> Result<Self> {
        let id = value.id.ok_or("image ID should not be nil")?;
        let config = value.config.map(|c| c.into()).unwrap_or_default();

        Ok(Self { id, config })
    }
}
