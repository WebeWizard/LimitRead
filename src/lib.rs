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
    match self.buf.read_until_lim(self.delim, &mut buf, &self.max) {
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
    match self.buf.read_line_lim(&mut buf, &self.max) {
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
  fn read_until_lim(&mut self, byte: u8, buf: &mut Vec<u8>, max: &usize) -> Result<usize> {
    read_until(self, byte, buf, max)
  }

  fn read_line_lim(&mut self, buf: &mut String, max: &usize) -> Result<usize> {
    append_to_string(buf, max, |b, m| read_until(self, b'\n', b, m))
  }

  fn split_lim(self, byte: u8, max: usize) -> Split<Self>
  where
    Self: Sized,
  {
    Split {
      buf: self,
      delim: byte,
      max: max,
    }
  }

  fn lines_lim(self, max: usize) -> Lines<Self>
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
  use crate::LimitRead;
  use std::io::BufReader;

  #[test]
  fn read_until_lim() {
    // prepare sample and reader
    let mut sample: Vec<u8> = vec![1; 10];
    sample[7] = ';' as u8;
    let mut buf_reader = BufReader::new(sample.as_slice());
    let mut buf: Vec<u8> = Vec::new();

    // should result in an error
    let short_lim = 3;
    buf_reader
      .read_until_lim(';' as u8, &mut buf, &short_lim)
      .is_err();

    // should result in ok(7)
    let long_lim = 10;
    let size = buf_reader
      .read_until_lim(';' as u8, &mut buf, &long_lim)
      .unwrap();
    assert_eq!(size, 8);
  }

  #[test]
  fn read_line_lim() {
    // prepare sample and reader
    let mut sample: Vec<u8> = vec![1; 10];
    sample[7] = '\n' as u8;
    let mut buf_reader = BufReader::new(sample.as_slice());
    let mut buf = String::new();
    // should result in an error
    let short_lim = 5;
    buf_reader.read_line_lim(&mut buf, &short_lim).is_err();

    // should result in ok(7)
    let long_lim = 10;
    let size = buf_reader.read_line_lim(&mut buf, &long_lim).unwrap();
    assert_eq!(size, 8);
  }

  #[test]
  fn split_lim() {
    // prepare sample and reader
    let mut sample: Vec<u8> = vec![1; 10];
    sample[3] = ';' as u8;
    sample[8] = ';' as u8;
    let buf_reader = BufReader::new(sample.as_slice());

    let short_lim = 4;
    let mut split_iter = buf_reader.split_lim(';' as u8, short_lim);
    // should succeed
    match split_iter.next() {
      Some(split_1) => match split_1 {
        Ok(_) => {}
        Err(_) => panic!("should have found the first split"),
      },
      None => panic!("should have read something"),
    }
    // should fail
    match split_iter.next() {
      Some(split_2) => {
        match split_2 {
          Ok(_) => panic!("should not have found the next split"),
          Err(_) => {} // we are expecting the error here
        }
      }
      None => panic!("should have read something"),
    }
  }

  #[test]
  fn lines_lim() {
    // prepare sample and reader
    let mut sample: Vec<u8> = vec![1; 10];
    sample[3] = '\n' as u8;
    sample[8] = '\n' as u8;
    let buf_reader = BufReader::new(sample.as_slice());

    let short_lim = 4;
    let mut line_iter = buf_reader.lines_lim(short_lim);
    // should succeed
    match line_iter.next() {
      Some(line_1_result) => match line_1_result {
        Ok(_) => {}
        Err(_) => panic!("should have found the first line"),
      },
      None => panic!("should have read something"),
    }
    // should fail
    match line_iter.next() {
      Some(line_2_result) => {
        match line_2_result {
          Ok(_) => panic!("should not have found the next line"),
          Err(_) => {} // we are expecting the error here
        }
      }
      None => panic!("should have read something"),
    }
  }
}
