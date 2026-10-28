use anyhow::{Context, Result, ensure};
use remdia_graph::{Control, Request};
use std::{
    io::{self, Read, Seek, SeekFrom},
    time::{Duration, Instant},
};
use symphonia::core::io::MediaSource;

struct Local {
    file: std::fs::File,
    control: Control,
}
fn interrupted(error: anyhow::Error) -> io::Error {
    io::Error::other(error.to_string())
}
impl Read for Local {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.control.check().map_err(interrupted)?;
        self.file.read(out)
    }
}
impl Seek for Local {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.control.check().map_err(interrupted)?;
        self.file.seek(to)
    }
}
impl MediaSource for Local {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        self.file.metadata().ok().map(|m| m.len())
    }
}
struct Network {
    client: reqwest::blocking::Client,
    url: String,
    position: u64,
    length: Option<u64>,
    seekable: bool,
    response: Option<reqwest::blocking::Response>,
    control: Control,
    timeout: Duration,
}
impl Network {
    fn connect(&mut self) -> io::Result<()> {
        let started = Instant::now();
        loop {
            self.control.check().map_err(interrupted)?;
            if started.elapsed() > self.timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HTTP response timed out",
                ));
            }
            let end = self.position.saturating_add(512 * 1024 - 1);
            let result = self
                .client
                .get(&self.url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .header(
                    reqwest::header::RANGE,
                    format!("bytes={}-{end}", self.position),
                )
                .send();
            let response = match result {
                Ok(r) => r,
                Err(e) if e.is_timeout() || e.is_connect() => continue,
                Err(e) => return Err(io::Error::other(e)),
            };
            if response.status().as_u16() == 416 && self.length.is_some_and(|l| self.position >= l)
            {
                self.response = None;
                return Ok(());
            }
            if !response.status().is_success() {
                return Err(io::Error::other(format!("HTTP {}", response.status())));
            }
            if response.status().as_u16() == 206 {
                let range = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| io::Error::other("missing Content-Range"))?;
                let (span, total) = range
                    .strip_prefix("bytes ")
                    .and_then(|s| s.split_once('/'))
                    .ok_or_else(|| io::Error::other("invalid Content-Range"))?;
                let first = span
                    .split_once('-')
                    .and_then(|(s, _)| s.parse::<u64>().ok())
                    .ok_or_else(|| io::Error::other("invalid Content-Range start"))?;
                if first != self.position {
                    return Err(io::Error::other("server returned the wrong byte range"));
                }
                self.length = total.parse().ok();
                self.seekable = true;
            } else {
                if self.position != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "source does not support HTTP Range seeking",
                    ));
                }
                self.length = response.content_length();
                self.seekable = false;
            }
            self.response = Some(response);
            return Ok(());
        }
    }
}
impl Read for Network {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let started = Instant::now();
        loop {
            self.control.check().map_err(interrupted)?;
            if self.length.is_some_and(|l| self.position >= l) {
                return Ok(0);
            }
            if started.elapsed() > self.timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HTTP body timed out",
                ));
            }
            if self.response.is_none() {
                self.connect()?;
            }
            let Some(response) = self.response.as_mut() else {
                return Ok(0);
            };
            match response.read(out) {
                Ok(0) => {
                    self.response = None;
                    if !self.seekable {
                        self.length = Some(self.position);
                        return Ok(0);
                    }
                }
                Ok(n) => {
                    self.position = self
                        .position
                        .checked_add(n as u64)
                        .ok_or_else(|| io::Error::other("HTTP offset overflow"))?;
                    return Ok(n);
                }
                Err(e) => {
                    self.response = None;
                    if !self.seekable {
                        return Err(e);
                    }
                }
            }
        }
    }
}
impl Seek for Network {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.control.check().map_err(interrupted)?;
        let position = match to {
            SeekFrom::Start(s) => i128::from(s),
            SeekFrom::Current(s) => i128::from(self.position) + i128::from(s),
            SeekFrom::End(s) => {
                i128::from(self.length.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "unknown source length")
                })?) + i128::from(s)
            }
        };
        let position = u64::try_from(position)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))?;
        if position == self.position {
            return Ok(position);
        }
        if !self.seekable && position != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "source is not seekable",
            ));
        }
        self.position = position;
        self.response = None;
        Ok(position)
    }
}
impl MediaSource for Network {
    fn is_seekable(&self) -> bool {
        self.seekable
    }
    fn byte_len(&self) -> Option<u64> {
        self.length
    }
}
pub fn open(request: &Request, control: Control) -> Result<Box<dyn MediaSource>> {
    control.check()?;
    if request.source.starts_with("http://") || request.source.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(500))
            .connect_timeout(Duration::from_millis(300))
            .build()?;
        let mut source = Network {
            client,
            url: request.source.clone(),
            position: 0,
            length: None,
            seekable: false,
            response: None,
            control,
            timeout: Duration::from_millis(request.timeout_ms),
        };
        source
            .connect()
            .with_context(|| format!("open HTTP source {}", request.source))?;
        control.check()?;
        Ok(Box::new(source))
    } else {
        ensure!(
            !request.source.contains("://"),
            "unsupported source protocol"
        );
        Ok(Box::new(Local {
            file: std::fs::File::open(&request.source)
                .with_context(|| format!("open {}", request.source))?,
            control,
        }))
    }
}
