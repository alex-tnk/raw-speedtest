//! Minimal JSON reader for the one small object `/meta` may return. Parsing
//! is strict enough to reject garbage and lenient about nothing else.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = Parser {
            s: s.as_bytes(),
            i: 0,
            depth: 0,
        };
        let v = p.value()?;
        p.ws();
        (p.i == p.s.len()).then_some(v)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn lit(&mut self, word: &[u8], v: Json) -> Option<Json> {
        if self.s[self.i..].starts_with(word) {
            self.i += word.len();
            Some(v)
        } else {
            None
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.depth += 1;
        if self.depth > 32 {
            return None;
        }
        self.ws();
        let v = match *self.s.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut kv = Vec::new();
                if !self.eat(b'}') {
                    loop {
                        self.ws();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return None;
                        }
                        kv.push((k, self.value()?));
                        if self.eat(b',') {
                            continue;
                        }
                        if self.eat(b'}') {
                            break;
                        }
                        return None;
                    }
                }
                Json::Obj(kv)
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                if !self.eat(b']') {
                    loop {
                        a.push(self.value()?);
                        if self.eat(b',') {
                            continue;
                        }
                        if self.eat(b']') {
                            break;
                        }
                        return None;
                    }
                }
                Json::Arr(a)
            }
            b'"' => Json::Str(self.string()?),
            b't' => self.lit(b"true", Json::Bool(true))?,
            b'f' => self.lit(b"false", Json::Bool(false))?,
            b'n' => self.lit(b"null", Json::Null)?,
            _ => {
                let start = self.i;
                while self.i < self.s.len()
                    && matches!(
                        self.s[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                let n = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                Json::Num(n.parse().ok()?)
            }
        };
        self.depth -= 1;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        if self.s.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            let cp = u32::from_str_radix(hex, 16).ok()?;
                            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                            let mut b = [0; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                        }
                        _ => return None,
                    }
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).ok()
    }
}

/// Escape a string for JSON output.
pub fn escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meta_shape() {
        let j = Json::parse(r#" {"a":1.5,"b":"x\"y","c":{"d":[true,null,-2e3]},"e":""} "#).unwrap();
        assert_eq!(j.get("a").unwrap().as_f64(), Some(1.5));
        assert_eq!(j.get("b").unwrap().as_str(), Some("x\"y"));
        assert_eq!(
            j.get("c").unwrap().get("d"),
            Some(&Json::Arr(vec![
                Json::Bool(true),
                Json::Null,
                Json::Num(-2000.0)
            ]))
        );
        assert_eq!(j.get("e").unwrap().as_str(), None);
        assert_eq!(Json::parse("{}"), Some(Json::Obj(vec![])));
        assert_eq!(Json::parse("{"), None);
        assert_eq!(Json::parse("<html>"), None);
    }

    #[test]
    fn escapes() {
        let mut s = String::new();
        escape("a\"b\n", &mut s);
        assert_eq!(s, r#""a\"b\n""#);
    }
}
