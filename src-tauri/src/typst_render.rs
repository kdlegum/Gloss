use log::warn;
use typst::diag::{FileError, FileResult, SourceDiagnostic, Warned};
use typst::foundations::{Bytes, Datetime};
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{compile, Library, LibraryExt, World};
use typst_kit::fonts::{FontSearcher, FontSlot, Fonts};

const PREVIEW_PREAMBLE: &str =
    "#set page(width: 640pt, height: 900pt, margin: (x: 18pt, y: 16pt))\n#set par(justify: false)\n";
const PREVIEW_IMPORT_ERROR: &str =
    "Imports and external file loads are not supported in Typst note previews.";

#[derive(serde::Serialize, Clone)]
pub struct TypstPreviewDocument {
    pub pages: Vec<String>,
}

pub struct TypstRenderer {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<FontSlot>,
}

impl TypstRenderer {
    pub fn new() -> Self {
        let Fonts { book, fonts } = FontSearcher::new().search();
        Self {
            library: LazyHash::new(<Library as LibraryExt>::default()),
            book: LazyHash::new(book),
            fonts,
        }
    }

    pub fn render_document(&self, source: &str) -> Result<TypstPreviewDocument, String> {
        let main = FileId::new_fake(VirtualPath::new("/gloss-preview.typ"));
        let world = PreviewWorld {
            renderer: self,
            main: Source::new(main, format!("{PREVIEW_PREAMBLE}\n{source}\n")),
        };

        let Warned { output, warnings } = compile::<PagedDocument>(&world);
        for warning in warnings {
            warn!(
                target: "gloss_lib::typst",
                "typst preview warning: {}",
                format_diagnostic(&warning)
            );
        }

        let document = output.map_err(format_diagnostics)?;
        Ok(TypstPreviewDocument {
            pages: document.pages.iter().map(typst_svg::svg).collect(),
        })
    }
}

struct PreviewWorld<'a> {
    renderer: &'a TypstRenderer,
    main: Source,
}

impl World for PreviewWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.renderer.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.renderer.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::Other(Some(PREVIEW_IMPORT_ERROR.into())))
        }
    }

    fn file(&self, _id: FileId) -> FileResult<Bytes> {
        Err(FileError::Other(Some(PREVIEW_IMPORT_ERROR.into())))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.renderer.fonts.get(index).and_then(FontSlot::get)
    }

    fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
        Datetime::from_ymd(1970, 1, 1)
    }
}

fn format_diagnostics(errors: impl IntoIterator<Item = SourceDiagnostic>) -> String {
    let mut lines = Vec::new();
    for diagnostic in errors {
        lines.push(format_diagnostic(&diagnostic));
    }
    if lines.is_empty() {
        "Typst preview failed.".to_string()
    } else {
        lines.join("\n\n")
    }
}

fn format_diagnostic(diagnostic: &SourceDiagnostic) -> String {
    let mut line = diagnostic.message.to_string();
    if !diagnostic.hints.is_empty() {
        let hints = diagnostic
            .hints
            .iter()
            .map(|hint| hint.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !hints.trim().is_empty() {
            line.push_str("\nHint: ");
            line.push_str(hints.trim());
        }
    }
    line
}
