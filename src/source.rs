//! Source files, byte spans and 1-based line/column mapping.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    pub fn empty(at: usize) -> Self {
        Span { start: at, end: at }
    }

    pub fn join(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

#[derive(Clone, Debug)]
pub struct Source {
    /// Path shown in diagnostics, relative to the package root, UTF-8.
    pub path: String,
    /// Absolute path used to reload or run the file.
    pub absolute: PathBuf,
    pub text: String,
    line_starts: Vec<usize>,
}

impl Source {
    pub fn new(path: impl Into<String>, text: impl Into<String>) -> Self {
        let path = path.into();
        let absolute = PathBuf::from(&path);
        Self::with_absolute(path, absolute, text)
    }

    pub fn with_absolute(
        path: impl Into<String>,
        absolute: PathBuf,
        text: impl Into<String>,
    ) -> Self {
        let text = text.into();
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Source {
            path: path.into(),
            absolute,
            text,
            line_starts,
        }
    }

    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.text.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        Position {
            line: (line + 1) as u32,
            col: (offset - self.line_starts[line] + 1) as u32,
        }
    }

    pub fn line_text(&self, line: u32) -> &str {
        let i = line.saturating_sub(1) as usize;
        let start = *self.line_starts.get(i).unwrap_or(&0);
        let end = self
            .line_starts
            .get(i + 1)
            .map(|e| e.saturating_sub(1))
            .unwrap_or(self.text.len());
        &self.text[start.min(self.text.len())..end.min(self.text.len())]
    }

    pub fn span(&self, start: usize, end: usize) -> Span {
        Span::new(start, end)
    }

    pub fn text_of(&self, span: Span) -> &str {
        &self.text[span.start.min(self.text.len())..span.end.min(self.text.len())]
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub files: Vec<Source>,
}

impl Sources {
    pub fn add(&mut self, source: Source) -> usize {
        self.files.push(source);
        self.files.len() - 1
    }

    pub fn get(&self, id: usize) -> &Source {
        &self.files[id]
    }

    pub fn get_by_path(&self, path: &Path) -> Option<&Source> {
        self.files
            .iter()
            .find(|s| s.absolute == path || s.path == path.to_string_lossy())
    }

    pub fn iter(&self) -> impl Iterator<Item = &Source> {
        self.files.iter()
    }
}
