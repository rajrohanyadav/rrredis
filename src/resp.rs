use std::io::{self, Write};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Simple(String),            // +OK\r\n
    Error(String),             // -ERR ...\r\n
    Integer(i64),              // :42\r\n
    Bulk(Option<Vec<u8>>),     // $3\r\nfoo\r\n  or  $-1\r\n (null)
    Array(Option<Vec<Value>>), // *2\r\n...    or  *-1\r\n (null)
}

impl Value {
    pub fn encode(&self, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Self::Simple(s) => write!(out, "+{s}\r\n"),
            Self::Error(e) => write!(out, "-{e}\r\n"),
            Self::Integer(i) => write!(out, ":{i}\r\n"),
            Self::Bulk(Some(b)) => {
                let len = b.len();
                write!(out, "${len}\r\n")?;
                out.write_all(b)?;
                write!(out, "\r\n")?;
                Ok(())
            }
            Self::Bulk(None) => write!(out, "$-1\r\n"),
            Self::Array(Some(a)) => {
                let len = a.len();
                write!(out, "*{len}\r\n")?;
                for item in a {
                    item.encode(out)?;
                }
                Ok(())
            }
            Self::Array(None) => write!(out, "*-1\r\n"),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use Value::*;

    #[test]
    fn encodes_correct_resp_value() {
        let cases: &[(Value, &[u8])] = &[
            (Simple("OK".to_string()), b"+OK\r\n"),
            (Error("ERR x".to_string()), b"-ERR x\r\n"),
            (Integer(0), b":0\r\n"),
            (Integer(-1), b":-1\r\n"),
            (Integer(i64::MAX), b":9223372036854775807\r\n"),
            (Integer(i64::MIN), b":-9223372036854775808\r\n"),
            (Bulk(Some(b"foo".to_vec())), b"$3\r\nfoo\r\n"),
            (Bulk(Some(b"".to_vec())), b"$0\r\n\r\n"),
            (Bulk(None), b"$-1\r\n"),
            (Bulk(Some(b"a\r\nb".to_vec())), b"$4\r\na\r\nb\r\n"),
            (Bulk(Some(vec![0, 255, 13, 10])), b"$4\r\n\0\xff\r\n\r\n"),
            (Array(Some(vec![])), b"*0\r\n"),
            (Array(None), b"*-1\r\n"),
            (
                Array(Some(vec![Array(Some(vec![Integer(1)])), Bulk(None)])),
                b"*2\r\n*1\r\n:1\r\n$-1\r\n",
            ),
        ];

        for (input, expected) in cases {
            let mut out = vec![];
            input.encode(&mut out).unwrap();
            assert_eq!(out, *expected, "test case: {input:?}");
        }
    }
}
