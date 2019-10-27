use std::fs::File;
use std::io::{Cursor, Read, Write};

use epub_builder::{EpubBuilder, EpubContent, TocElement, ZipLibrary};
use failure::{Error, ResultExt};
use handlebars::Handlebars;
use mdbook::book::{BookItem, Chapter};
use mdbook::renderer::RenderContext;
use mdbook::theme::Theme;
use regex::Regex;
use serde_json::json;
use std::env;
use std::path::PathBuf;

use crate::config::Config;
use crate::resources::{self, Asset};
use crate::utils::ResultExt as _;
use crate::DEFAULT_CSS;

/// The actual EPUB book renderer.
#[derive(Debug)]
pub struct Generator<'a> {
    ctx: &'a RenderContext,
    builder: EpubBuilder<ZipLibrary>,
    config: Config,
    hbs: Handlebars,
}

impl<'a> Generator<'a> {
    pub fn new(ctx: &'a RenderContext) -> Result<Generator<'a>, Error> {
        let builder = EpubBuilder::new(ZipLibrary::new().sync()?).sync()?;

        let config = Config::from_render_context(ctx)?;

        let mut theme_dir: PathBuf;
        let env_theme_dir = env::var("MDBOOKEPUB_THEME_DIR");
        if env_theme_dir.is_ok() {
            theme_dir = PathBuf::from(env_theme_dir.unwrap());
        } else {
            theme_dir = env::current_exe().unwrap().parent().unwrap().to_path_buf();
            theme_dir.push("theme");
        }
        log::debug!("theme_dir: {}", theme_dir.display());
        if !theme_dir.exists() {
            panic!("theme dir \"{}\" doesn't exist.", theme_dir.display());
        }

        let theme = Theme::new(theme_dir);

        let mut hbs = Handlebars::new();
        hbs.register_template_string("index", String::from_utf8(theme.index.clone())?)?;

        Ok(Generator {
            builder,
            ctx,
            config,
            hbs
        })
    }

    fn populate_metadata(&mut self) -> Result<(), Error> {
        self.builder.metadata("generator", "mdbook-epub").sync()?;

        if let Some(title) = self.ctx.config.book.title.clone() {
            self.builder.metadata("title", title).sync()?;
        }
        if let Some(desc) = self.ctx.config.book.description.clone() {
            self.builder.metadata("description", desc).sync()?;
        }

        if !self.ctx.config.book.authors.is_empty() {
            self.builder
                .metadata("author", self.ctx.config.book.authors.join(", "))
                .sync()?;
        }

        Ok(())
    }

    pub fn generate<W: Write>(mut self, writer: W) -> Result<(), Error> {
        log::info!("Generating the EPUB book");

        self.populate_metadata()?;
        self.generate_chapters()?;

        self.embed_stylesheets()?;
        self.additional_assets()?;
        self.builder.generate(writer).sync()?;

        Ok(())
    }

    fn generate_chapters(&mut self) -> Result<(), Error> {
        log::debug!("Rendering Chapters");

        for item in self.ctx.book.iter() {
            if let BookItem::Chapter(ref ch) = *item {
                // iter() gives us an iterator over every node in the tree
                // but we only want the top level here so we can recursively
                // visit the chapters.
                log::debug!("Adding chapter \"{}\"", ch);
                self.add_chapter(ch)?;
            }
        }

        Ok(())
    }

    fn add_chapter(&mut self, ch: &Chapter) -> Result<(), Error> {
        let html = mdbook::utils::render_markdown(&ch.content, /*curly_quotes=*/false);
        let html = self.fix_html(html);
        let html = self.hbs.render("index", &json!({"content": html}))?;
        let data = Cursor::new(Vec::from(html));

        let path = ch.path.with_extension("html").display().to_string();
        let mut content = EpubContent::new(path, data).title(format!("{}", ch));

        let level = ch.number.as_ref().map(|n| n.len() as i32 - 1).unwrap_or(0);
        content = content.level(level);

        // unfortunately we need to do two passes through `ch.sub_items` here.
        // The first pass will add each sub-item to the current chapter's toc
        // and the second pass actually adds the sub-items to the book.
        for sub_item in &ch.sub_items {
            if let BookItem::Chapter(ref sub_ch) = *sub_item {
                let child_path = sub_ch.path.with_extension("html").display().to_string();
                content = content.child(TocElement::new(child_path, format!("{}", sub_ch)));
            }
        }

        self.builder.add_content(content).sync()?;

        Ok(())
    }

    /// Generate the stylesheet and add it to the document.
    fn embed_stylesheets(&mut self) -> Result<(), Error> {
        log::debug!("Embedding stylesheets");

        let stylesheet = self
            .generate_stylesheet()
            .context("Unable to generate stylesheet")?;
        self.builder.stylesheet(stylesheet.as_slice()).sync()?;

        Ok(())
    }

    fn additional_assets(&mut self) -> Result<(), Error> {
        log::debug!("Embedding additional assets");

        let assets = resources::find(self.ctx)
            .context("Inspecting the book for additional assets failed")?;

        for asset in assets {
            log::debug!("Embedding {}", asset.filename.display());
            self.load_asset(&asset)
                .with_context(|_| format!("Couldn't load {}", asset.filename.display()))?;
        }

        Ok(())
    }

    fn load_asset(&mut self, asset: &Asset) -> Result<(), Error> {
        let content = File::open(&asset.location_on_disk).context("Unable to open asset")?;

        let mt = asset.mimetype.to_string();

        // Change '\\' to '/'
        let filename = asset.filename.to_str().unwrap();
        let filename = str::replace(&filename, "\\", "/");

        self.builder
            .add_resource(filename, content, mt)
            .sync()?;

        Ok(())
    }

    /// Concatenate all provided stylesheets into one long stylesheet.
    fn generate_stylesheet(&self) -> Result<Vec<u8>, Error> {
        let mut stylesheet = Vec::new();

        if self.config.use_default_css {
            stylesheet.extend(DEFAULT_CSS.as_bytes());
        }

        for additional_css in &self.config.additional_css {
            let mut f = File::open(&additional_css)
                .with_context(|_| format!("Unable to open {}", additional_css.display()))?;
            f.read_to_end(&mut stylesheet)
                .context("Error reading stylesheet")?;
        }

        Ok(stylesheet)
    }

    fn fix_html(&self, html: String) -> String {
        let html = self.fix_img(html);
        return html;
    }

    fn fix_img(&self, html: String) -> String {
        lazy_static! {
            static ref IMG: Regex =
                    Regex::new(r"(?P<img><img\s+[^>]*/>)").unwrap();
        }

        // As epub standard, img should be inside a block element.
        // So here, always put <img ... /> into a <p>.
        return IMG.replace_all(&html, "<p>$img</p>").to_string();
    }
}
