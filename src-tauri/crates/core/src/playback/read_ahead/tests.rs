use super::*;
use std::sync::mpsc;

/// A reader handing out whatever the test sends it, and blocking in
/// between: a transfer the test controls.
struct Gated(mpsc::Receiver<Vec<u8>>, Vec<u8>);

impl Read for Gated {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(next) => self.1 = next,
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.1.len());
        buf[..n].copy_from_slice(&self.1[..n]);
        self.1.drain(..n);
        Ok(n)
    }
}

/// The point of it: the start of the file can be read while the rest
/// is still on its way.
#[test]
fn the_start_is_readable_before_the_transfer_ends() {
    let (send, gate) = mpsc::channel();
    let mut source = ReadAhead::spawn(Gated(gate, Vec::new()), None, 8).unwrap();
    send.send(b"head".to_vec()).unwrap();

    let mut head = [0_u8; 4];
    source.read_exact(&mut head).unwrap();
    assert_eq!(&head, b"head");

    send.send(b"tail".to_vec()).unwrap();
    drop(send);
    let mut rest = Vec::new();
    source.read_to_end(&mut rest).unwrap();
    assert_eq!(rest, b"tail");
}

#[test]
fn a_seek_ahead_of_the_transfer_waits_for_the_bytes() {
    let (send, gate) = mpsc::channel();
    let mut source = ReadAhead::spawn(Gated(gate, Vec::new()), None, 10).unwrap();
    source.seek(SeekFrom::Start(6)).unwrap();
    let reader = std::thread::spawn(move || {
        let mut four = [0_u8; 4];
        source.read_exact(&mut four).unwrap();
        four
    });
    send.send(b"012345".to_vec()).unwrap();
    send.send(b"6789".to_vec()).unwrap();
    assert_eq!(&reader.join().unwrap(), b"6789");
}

#[test]
fn seeks_from_the_end_and_from_here_land_where_asked() {
    let mut source = ReadAhead::spawn(io::Cursor::new(b"0123456789".to_vec()), None, 10).unwrap();
    assert_eq!(source.seek(SeekFrom::End(-3)).unwrap(), 7);
    assert_eq!(source.seek(SeekFrom::Current(-2)).unwrap(), 5);
    let mut two = [0_u8; 2];
    source.read_exact(&mut two).unwrap();
    assert_eq!(&two, b"56");
    assert!(source.seek(SeekFrom::Current(-100)).is_err());
}

/// A read far ahead of the transfer (an index at the end of the file,
/// a seek into the middle) goes to the file at once rather than
/// waiting for everything before it.
#[test]
fn a_read_far_ahead_goes_to_the_file() {
    let data: Vec<u8> = (0..3 * NEAR as usize).map(|i| (i % 251) as u8).collect();
    let (_send, gate) = mpsc::channel::<Vec<u8>>();
    let direct: Box<dyn RandomAccess> = Box::new(io::Cursor::new(data.clone()));
    let mut source =
        ReadAhead::spawn(Gated(gate, Vec::new()), Some(direct), data.len() as u64).unwrap();

    // The transfer never delivers a byte, and the read still returns.
    source.seek(SeekFrom::End(-16)).unwrap();
    let mut tail = [0_u8; 16];
    source.read_exact(&mut tail).unwrap();
    assert_eq!(tail, data[data.len() - 16..]);
}

/// A transfer that fails gives what arrived, then the error rather
/// than a quiet end of file.
#[test]
fn a_failed_transfer_surfaces_after_what_arrived() {
    let mut source = ReadAhead::spawn(Failing(false), None, 6).unwrap();
    let mut three = [0_u8; 3];
    source.read_exact(&mut three).unwrap();
    assert_eq!(&three, b"abc");
    let err = source.read(&mut three).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
    assert!(err.to_string().contains("link dropped"));
}

/// With a second handle, a transfer that fails is not the end: the
/// rest is read from the file itself.
#[test]
fn a_failed_transfer_falls_back_to_the_file() {
    let direct: Box<dyn RandomAccess> = Box::new(io::Cursor::new(b"abcdef".to_vec()));
    let mut source = ReadAhead::spawn(Failing(false), Some(direct), 6).unwrap();
    let mut all = Vec::new();
    source.read_to_end(&mut all).unwrap();
    assert_eq!(all, b"abcdef");
}

/// Three bytes, then a dropped link.
struct Failing(bool);

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if std::mem::replace(&mut self.0, true) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "link dropped",
            ));
        }
        buf[..3].copy_from_slice(b"abc");
        Ok(3)
    }
}
