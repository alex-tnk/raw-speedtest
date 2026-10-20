//! Tooltip content: what a hover shows, as rows. Shells lay it out.

#[derive(Clone, Debug, PartialEq)]
pub enum TipRow {
    Title(String),
    Pair(String, String),
    /// How a value is measured; wrapped prose.
    Note(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tip {
    pub rows: Vec<TipRow>,
}

impl Tip {
    pub fn title(mut self, s: impl Into<String>) -> Tip {
        self.rows.push(TipRow::Title(s.into()));
        self
    }
    pub fn pair(mut self, k: impl Into<String>, v: impl Into<String>) -> Tip {
        self.push_pair(k, v);
        self
    }
    pub fn note(mut self, s: impl Into<String>) -> Tip {
        self.rows.push(TipRow::Note(s.into()));
        self
    }
    pub fn push_pair(&mut self, k: impl Into<String>, v: impl Into<String>) {
        self.rows.push(TipRow::Pair(k.into(), v.into()));
    }
    // Not every shell uses every part of the view layer.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}
