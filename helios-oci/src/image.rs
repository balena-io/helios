use std::collections::HashMap;

use bollard::query_parameters::{
    CreateImageOptions, ListImagesOptions, RemoveImageOptions, TagImageOptions,
};
use futures_lite::StreamExt;

use super::util::types::{ImageUri, InvalidImageUriError};
use super::{Client, Credentials, Error, Result, WithContext};

use bollard::secret::{ImageInspect, ImageSummary};

#[derive(Debug, Clone)]
pub struct Image<'a>(&'a Client);

impl<'a> Image<'a> {
    pub fn new(client: &'a Client) -> Self {
        Self(client)
    }
}

impl Image<'_> {
    /// Returns an iterator on the list of images on the server.
    ///
    /// Note that it uses a different, smaller representation of an image than
    /// inspecting a single image.
    pub async fn list(&self) -> Result<List> {
        let opts = ListImagesOptions {
            all: true,
            ..Default::default()
        };

        let res = self.0.inner().list_images(Some(opts)).await;
        let images = res.map_err(Error::with_context("failed to list images"))?;

        Ok(List(images))
    }

    /// Tags an image so that it becomes part of a repository.
    pub async fn tag(&self, name: &str, new_name: &ImageUri) -> Result<()> {
        let repo = new_name.repo();
        let tag = new_name.tag().clone();
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
        let opts = Some(CreateImageOptions {
            from_image: Some(image.clone().into()),
            ..Default::default()
        });

        let mut stream = self.0.inner().create_image(opts, None, creds);
        while let Some(progress) = stream.next().await {
            // TODO: report progress. This requires https://github.com/balena-io-modules/mahler-rs/issues/43
            let _ = progress
                .map_err(Error::from)
                .with_context(|| format!("failed to pull image {}", image.as_str()))?;
        }

        Ok(())
    }

    /// Returns low-level information about an image.
    pub async fn inspect(&self, image: &ImageUri) -> Result<LocalImage> {
        let res = self.0.inner().inspect_image(image.as_str()).await;
        let info = res
            .map_err(Error::from)
            .with_context(|| format!("failed to inspect image {}", image.as_str()))?;

        Ok((&info).into())
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

    /// User-defined key/value metadata.
    pub labels: HashMap<String, String>,
}

// by ref in order to clone only what's necessary to build LocalImage.
impl<'a> From<&'a ImageSummary> for LocalImage {
    fn from(value: &'a ImageSummary) -> Self {
        let id = value.id.clone();
        let labels = value.labels.clone();
        Self { id, labels }
    }
}

impl<'a> From<&'a ImageInspect> for LocalImage {
    fn from(value: &'a ImageInspect) -> Self {
        // FIXME: when is ID nil?
        let id = value.id.clone().expect("image ID should not be nil");
        let labels = value
            .config
            .as_ref()
            .and_then(|c| c.labels.clone())
            .unwrap_or_default();
        Self { id, labels }
    }
}

#[derive(Debug, Clone)]
pub struct List(Vec<ImageSummary>);

type ListItem = std::result::Result<(ImageUri, LocalImage), InvalidImageUriError>;

impl List {
    pub fn iter(&self) -> impl Iterator<Item = ListItem> + '_ {
        self.0.iter().flat_map(|image| {
            image
                .repo_tags
                .iter()
                .map(|tag| Ok((tag.parse()?, image.into())))
        })
    }
}
