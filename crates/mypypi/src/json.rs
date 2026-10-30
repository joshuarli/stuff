#![forbid(unsafe_code)]

#[derive(Debug, Clone)]
pub enum JVal {
    Null,
    Bool(bool),
    Str(String),
    Arr(Vec<JVal>),
    Obj(Vec<(String, JVal)>),
}

impl JVal {
    pub fn get(&self, key: &str) -> Option<&JVal> {
        match self {
            JVal::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            JVal::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&[JVal]> {
        match self {
            JVal::Arr(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_obj(&self) -> Option<&[(String, JVal)]> {
        match self {
            JVal::Obj(o) => Some(o),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, JVal::Null)
    }
    pub fn as_num(&self) -> Option<&str> {
        // numbers are stored as Str by this parser
        match self {
            JVal::Str(s) => Some(s),
            _ => None,
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self { b: s.as_bytes(), pos: 0 }
    }
    fn ws(&mut self) {
        while self.pos < self.b.len() && matches!(self.b[self.pos], b' ' | b'\t' | b'\n' | b'\r') {
            self.pos += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }
    fn expect(&mut self, c: u8) -> std::result::Result<(), String> {
        self.ws();
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{c}' at offset {}", self.pos))
        }
    }
    fn parse_value(&mut self) -> std::result::Result<JVal, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.parse_obj(),
            Some(b'[') => self.parse_arr(),
            Some(b'"') => Ok(JVal::Str(self.parse_string()?)),
            Some(b't') => self.parse_lit("true", JVal::Bool(true)),
            Some(b'f') => self.parse_lit("false", JVal::Bool(false)),
            Some(b'n') => self.parse_lit("null", JVal::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!("unexpected character '{c}' at offset {}", self.pos)),
            None => Err("unexpected end of JSON".to_string()),
        }
    }
    fn parse_lit(&mut self, lit: &str, v: JVal) -> std::result::Result<JVal, String> {
        if self.b[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(v)
        } else {
            Err(format!("invalid literal at offset {}", self.pos))
        }
    }
    fn parse_number(&mut self) -> std::result::Result<JVal, String> {
        let start = self.pos;
        while self.pos < self.b.len()
            && matches!(self.b[self.pos], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
        {
            self.pos += 1;
        }
        let s = std::str::from_utf8(&self.b[start..self.pos]).map_err(|e| e.to_string())?;
        Ok(JVal::Str(s.to_string()))
    }
    fn parse_string(&mut self) -> std::result::Result<String, String> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.pos).ok_or("unterminated string")?;
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.pos).ok_or("unterminated escape")?;
                    self.pos += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            if self.pos + 4 > self.b.len() {
                                return Err("bad unicode escape".to_string());
                            }
                            let hex = std::str::from_utf8(&self.b[self.pos..self.pos + 4])
                                .map_err(|e| e.to_string())?;
                            let cp = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                            self.pos += 4;
                            let ch = if (0xD800..0xDC00).contains(&cp)
                                && self.b.get(self.pos..self.pos + 2) == Some(b"\\u")
                            {
                                let hex2 = std::str::from_utf8(&self.b[self.pos + 2..self.pos + 6])
                                    .map_err(|e| e.to_string())?;
                                let lo = u32::from_str_radix(hex2, 16).map_err(|e| e.to_string())?;
                                self.pos += 6;
                                let full = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                char::from_u32(full).ok_or("bad unicode escape")?
                            } else {
                                char::from_u32(cp).ok_or("bad unicode escape")?
                            };
                            out.push(ch);
                        }
                        _ => return Err("bad escape".to_string()),
                    }
                }
                _ => {
                    let s = std::str::from_utf8(self.b).map_err(|e| e.to_string())?;
                    let ch = s[self.pos - 1..].chars().next().ok_or("bad utf8")?;
                    out.push(ch);
                    self.pos += ch.len_utf8() - 1;
                }
            }
        }
        Ok(out)
    }
    fn parse_arr(&mut self) -> std::result::Result<JVal, String> {
        self.pos += 1;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JVal::Arr(out));
        }
        loop {
            out.push(self.parse_value()?);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or ']' at offset {}", self.pos)),
            }
        }
        Ok(JVal::Arr(out))
    }
    fn parse_obj(&mut self) -> std::result::Result<JVal, String> {
        self.pos += 1;
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JVal::Obj(out));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected string key at offset {}", self.pos));
            }
            let k = self.parse_string()?;
            self.expect(b':')?;
            let v = self.parse_value()?;
            out.push((k, v));
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or '}}' at offset {}", self.pos)),
            }
        }
        Ok(JVal::Obj(out))
    }
}

pub fn parse(text: &str) -> std::result::Result<JVal, String> {
    let mut p = Parser::new(text);
    let v = p.parse_value()?;
    p.ws();
    if p.pos != p.b.len() {
        return Err("trailing data".to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_nested() {
        let v = parse("{\"a\": [1, \"x\\u00e9\", null, true], \"b\": {}}").unwrap();
        assert_eq!(v.get("a").and_then(|a| a.as_arr()).map(|a| a.len()), Some(4));
        assert!(v.get("b").and_then(|b| b.as_obj()).map(|o| o.is_empty()).unwrap());
    }
}
