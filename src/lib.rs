use std::io::{BufRead, Error, ErrorKind, Result};

struct Guard<'a> {
  buf: &'a mut Vec<u8>,
  len: usize,
}

impl Drop for Guard<'_> {
  fn drop(&mut self) {
    unsafe {
      self.buf.set_len(self.len);
    }
  }
}

// A few methods below (read_to_string, read_line) will append data into a
// `String` buffer, but we need to be pretty careful when doing this. The
// implementation will just call `.as_mut_vec()` and then delegate to a
// byte-oriented reading method, but we must ensure that when returning we never
// leave `buf` in a state such that it contains invalid UTF-8 in its bounds.
//
// To this end, we use an RAII guard (to protect against panics) which updates
// the length of the string when it is dropped. This guard initially truncates
// the string to the prior length and only after we've validated that the
// new contents are valid UTF-8 do we allow it to set a longer length.
//
// The unsafety in this function is twofold:
//
// 1. We're looking at the raw bytes of `buf`, so we take on the burden of UTF-8
//    checks.
// 2. We're passing a raw buffer to the function `f`, and it is expected that
//    the function only *appends* bytes to the buffer. We'll get undefined
//    behavior if existing bytes are overwritten to have non-UTF-8 data.
fn append_to_string<F>(buf: &mut String, max: &usize, f: F) -> Result<usize>
where
  F: FnOnce(&mut Vec<u8>, &usize) -> Result<usize>,
{
  unsafe {
    let mut g = Guard {
      len: buf.len(),
      buf: buf.as_mut_vec(),
    };
    let ret = f(g.buf, max);
    if std::str::from_utf8(&g.buf[g.len..]).is_err() {
      ret.and_then(|_| {
        Err(Error::new(
          ErrorKind::InvalidData,
          "stream did not contain valid UTF-8",
        ))
      })
    } else {
      g.len = g.buf.len();
      ret
    }
  }
}

fn read_until<R: BufRead + ?Sized>(
  r: &mut R,
  delim: u8,
  buf: &mut Vec<u8>,
  max: &usize,
) -> Result<usize> {
  let mut read = 0;
  loop {
    let (done, used) = {
      let available = match r.fill_buf() {
        Ok(n) => n,
        Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
        Err(e) => return Err(e),
      };
      match memchr::memchr(delim, available) {
        Some(i) => {
          if &(read + i + 1) > max {
            return Err(Error::from(ErrorKind::NotFound));
          }
          buf.extend_from_slice(&available[..=i]);
          (true, i + 1)
        }
        None => {
          buf.extend_from_slice(available);
          (false, available.len())
        }
      }
    };
    r.consume(used);
    read += used;
    if done || used == 0 {
      return Ok(read);
    }
  }
}

#[derive(Debug)]
pub struct Split<B> {
  buf: B,
  delim: u8,
  max: usize,
}

impl<B: LimitRead> Iterator for Split<B> {
  type Item = Result<Vec<u8>>;

  fn next(&mut self) -> Option<Result<Vec<u8>>> {
    let mut buf = Vec::new();
    match LimitRead::read_until(&mut self.buf, self.delim, &mut buf, &self.max) {
      Ok(0) => None,
      Ok(_n) => {
        if buf[buf.len() - 1] == self.delim {
          buf.pop();
        }
        Some(Ok(buf))
      }
      Err(e) => Some(Err(e)),
    }
  }
}

#[derive(Debug)]
pub struct Lines<B> {
  buf: B,
  max: usize,
}

impl<B: LimitRead> Iterator for Lines<B> {
  type Item = Result<String>;

  fn next(&mut self) -> Option<Result<String>> {
    let mut buf = String::new();
    match LimitRead::read_line(&mut self.buf, &mut buf, &self.max) {
      Ok(0) => None,
      Ok(_n) => {
        if buf.ends_with("\n") {
          buf.pop();
          if buf.ends_with("\r") {
            buf.pop();
          }
        }
        Some(Ok(buf))
      }
      Err(e) => Some(Err(e)),
    }
  }
}

pub trait LimitRead: BufRead {
  fn read_until(&mut self, byte: u8, buf: &mut Vec<u8>, max: &usize) -> Result<usize> {
    read_until(self, byte, buf, max)
  }

  fn read_line(&mut self, buf: &mut String, max: &usize) -> Result<usize> {
    append_to_string(buf, max, |b, m| read_until(self, b'\n', b, m))
  }

  fn split(self, byte: u8, max: usize) -> Split<Self>
  where
    Self: Sized,
  {
    Split {
      buf: self,
      delim: byte,
      max: max,
    }
  }

  fn lines(self, max: usize) -> Lines<Self>
  where
    Self: Sized,
  {
    Lines {
      buf: self,
      max: max,
    }
  }
}

impl<T: BufRead> LimitRead for T {}

#[cfg(test)]
mod tests {
  #[test]
  fn it_works() {
    assert_eq!(2 + 2, 4);
  }
}
