use failure::{self, Error, ResultExt};
use mdbook::book::BookItem;
use mdbook::renderer::RenderContext;
use mime_guess::{self, Mime};
use pulldown_cmark::{Event, Parser, Tag};
use regex::Regex;
use std::path::{Path, PathBuf};

pub(crate) fn find(ctx: &RenderContext) -> Result<Vec<Asset>, Error> {
    let mut assets = Vec::new();
    let src_dir = ctx
        .root
        .join(&ctx.config.book.src)
        .canonicalize()
        .context("Unable to canonicalize the src directory")?;

    for section in ctx.book.iter() {
        if let BookItem::Chapter(ref ch) = *section {
            log::trace!("Searching {} for links and assets", ch);

            let mut full_path = src_dir.to_path_buf();
            for s in ch.path.to_str().unwrap().split("/") {
                full_path.push(s);
            }
            full_path.pop();
            let found = assets_in_markdown(&ch.content, &full_path)?;

            for full_filename in found {
                let relative = full_filename.strip_prefix(&src_dir).unwrap();
                assets.push(Asset::new(relative, &full_filename));
            }
        }
    }

    Ok(assets)
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Asset {
    /// The asset's absolute location on disk.
    pub(crate) location_on_disk: PathBuf,
    /// The asset's filename relative to the `src/` directory.
    pub(crate) filename: PathBuf,
    pub(crate) mimetype: Mime,
}

impl Asset {
    fn new<P, Q>(filename: P, absolute_location: Q) -> Asset
    where
        P: Into<PathBuf>,
        Q: Into<PathBuf>,
    {
        let location_on_disk = absolute_location.into();
        let mt = mime_guess::from_path(&location_on_disk).first_or_octet_stream();

        Asset {
            location_on_disk,
            filename: filename.into(),
            mimetype: mt,
        }
    }
}

fn assets_in_markdown(src: &str, parent_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut found = Vec::new();
    for event in Parser::new(src) {
        match event {
            Event::Start(Tag::Image(_, dest, _)) => {
                found.push(dest.to_string());
            }
            Event::Html(html) => {
                lazy_static! {
                    static ref HTML_LINK: Regex =
                        Regex::new(r#"(<(?:a|img) [^>]*?(?:src|href)=")([^"]+?)""#).unwrap();
                }
                let captures = HTML_LINK.captures(&html);
                if !captures.is_none() {
                    let path = captures.unwrap().get(2);
                    if !path.is_none() {
                        found.push(path.unwrap().as_str().to_string());
                    }
                }
            }
            _ => {
            }
        }
    }

    // TODO: Allow linked images to be either a URL or path on disk

    // I'm assuming you'd just determine if each link is a URL or filename so
    // the `find()` function can put together a deduplicated list of URLs and
    // try to download all of them (in parallel?) to a temporary location. It'd
    // be nice if we could have some sort of caching mechanism by using the
    // destination directory (hash the URL and store it as
    // `book/epub/cache/$hash.$ext`?).
    let mut assets = Vec::new();

    for link in found {
        let mut filename = parent_dir.to_path_buf();
        for s in link.split("/") {
            filename.push(s);
        }
        let filename = filename.canonicalize().with_context(|_| {
            format!(
                "Unable to fetch the canonical path for {}",
                filename.display()
            )
        })?;

        if !filename.is_file() {
            return Err(failure::err_msg(format!(
                "Asset was not a file, {}",
                filename.display()
            )));
        }

        assets.push(filename);
    }

    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_images() {
        let parent_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/dummy/src");
        let src =
            "![Image 1](./rust-logo.png)\n[a link](to/nowhere) ![Image 2][2]\n\n[2]: reddit.svg\n";
        let should_be = vec![
            parent_dir.join("rust-logo.png").canonicalize().unwrap(),
            parent_dir.join("reddit.svg").canonicalize().unwrap(),
        ];

        let got = assets_in_markdown(src, &parent_dir).unwrap();

        assert_eq!(got, should_be);
    }
}
