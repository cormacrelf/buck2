/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

use std::fmt;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use allocative::Allocative;
use base64::Engine;
use buck2_common::cas_digest::CasDigestConfig;
use buck2_common::cas_digest::DigestAlgorithmFamily;
use buck2_common::cas_digest::SHA1_SIZE;
use buck2_common::cas_digest::SHA256_SIZE;
use buck2_common::file_ops::metadata::FileDigest;
use buck2_common::file_ops::metadata::TrackedFileDigest;
use buck2_core::fs::project::ProjectRoot;
use buck2_core::fs::project_rel_path::ProjectRelativePath;
use buck2_error::BuckErrorContext;
use buck2_fs::error::IoResultExt;
use buck2_fs::fs_util;
use buck2_http::HttpClient;
use buck2_http::retries::HttpError;
use buck2_http::retries::HttpErrorForRetry;
use buck2_http::retries::IntoBuck2Error;
use buck2_http::retries::http_retry;
use bytes::Bytes;
use digest::DynDigest;
use dupe::Dupe;
use futures::StreamExt;
use futures::stream::Stream;
use hyper::Response;
use pagable::Pagable;
use sha1::Digest;
use sha1::Sha1;
use sha2::Sha256;
use sha2::Sha384;
use sha2::Sha512;
use smallvec::SmallVec;

use crate::digest_config::DigestConfig;

/// What a download must match. `sha1` and `sha256` are lowercase hex and must each match;
/// `sri` passes if any of its digests matches.
#[derive(Debug, Clone, Dupe, Allocative, Pagable)]
pub struct Checksum {
    sha1: Option<Arc<str>>,
    sha256: Option<Arc<str>>,
    sri: Option<Arc<SriChecksum>>,
}

/// The digests of a Subresource Integrity string that apply: those of its strongest algorithm,
/// as lowercase hex.
#[derive(Debug, Allocative, Pagable)]
struct SriChecksum {
    algorithm: SriAlgorithm,
    digests: Vec<Arc<str>>,
}

/// In order of strength. SHA-1 is not an SRI algorithm, but npm lockfiles still carry it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Allocative, Pagable
)]
enum SriAlgorithm {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl SriAlgorithm {
    fn parse(algorithm: &str) -> Option<Self> {
        match algorithm.to_ascii_lowercase().as_str() {
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    fn digest_len(self) -> usize {
        match self {
            Self::Sha1 => SHA1_SIZE,
            Self::Sha256 => SHA256_SIZE,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    fn hasher(self) -> Box<dyn DynDigest + Send> {
        match self {
            Self::Sha1 => Box::new(Sha1::new()),
            Self::Sha256 => Box::new(Sha256::new()),
            Self::Sha384 => Box::new(Sha384::new()),
            Self::Sha512 => Box::new(Sha512::new()),
        }
    }

    fn digest_family(self) -> Option<DigestAlgorithmFamily> {
        match self {
            Self::Sha1 => Some(DigestAlgorithmFamily::Sha1),
            Self::Sha256 => Some(DigestAlgorithmFamily::Sha256),
            Self::Sha384 | Self::Sha512 => None,
        }
    }
}

#[derive(buck2_error::Error, Debug)]
#[buck2(tag = Input)]
enum DownloadFileError {
    #[error("Must pass in at least one checksum (e.g. `sha1 = ...` or `sri = ...`)")]
    MissingChecksum,
    #[error("Invalid digest for `{digest_type}` argument, expected length of {expected_len} but got {}, digest `{digest}`", digest.len())]
    InvalidDigestLength {
        digest: String,
        expected_len: usize,
        digest_type: &'static str,
    },
    #[error(
        "Invalid digest for `{digest_type}` argument, expected 0-9 a-z hex characters, but got `{bad_char}`, digest `{digest}`"
    )]
    InvalidDigestCharacter {
        digest: String,
        bad_char: char,
        digest_type: &'static str,
    },
    #[error(
        "No usable hash in `sri` `{0}`: expected `<algorithm>-<base64 digest>` with algorithm sha512, sha384, sha256 or sha1"
    )]
    SriNothingUsable(String),
    #[error(
        "Invalid `sri` hash `{hash}`: a {algorithm} digest is {expected_len} bytes of base64, which this is not"
    )]
    SriInvalidDigest {
        hash: String,
        algorithm: &'static str,
        expected_len: usize,
    },
}

/// Parse a Subresource Integrity string as the W3C spec does
/// (<https://www.w3.org/TR/SRI/#parse-metadata>), then keep the hashes of its strongest
/// algorithm (<https://www.w3.org/TR/SRI/#get-the-strongest-metadata>).
///
/// The string is a whitespace-separated list of `<algorithm>-<base64 digest>[?<options>]`.
/// Options are ignored, as are hashes whose algorithm is unknown or that have no `-`. A hash of
/// a known algorithm whose digest is not base64 of the right length can never match, so it is
/// an error here rather than a failed download later.
fn parse_sri(sri: &str) -> buck2_error::Result<SriChecksum> {
    let mut strongest: Option<SriChecksum> = None;
    for hash in sri.split_ascii_whitespace() {
        let hash_without_options = hash.split_once('?').map_or(hash, |(h, _)| h);
        let Some((algorithm, digest)) = hash_without_options.split_once('-') else {
            continue;
        };
        let Some(algorithm) = SriAlgorithm::parse(algorithm) else {
            continue;
        };
        let invalid = || DownloadFileError::SriInvalidDigest {
            hash: hash.to_owned(),
            algorithm: algorithm.name(),
            expected_len: algorithm.digest_len(),
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(digest)
            .map_err(|_| invalid())?;
        if bytes.len() != algorithm.digest_len() {
            return Err(invalid().into());
        }
        let digest = Arc::from(hex::encode(bytes));
        match &mut strongest {
            Some(s) if s.algorithm == algorithm => s.digests.push(digest),
            Some(s) if s.algorithm > algorithm => {}
            _ => {
                strongest = Some(SriChecksum {
                    algorithm,
                    digests: vec![digest],
                })
            }
        }
    }
    strongest.ok_or_else(|| DownloadFileError::SriNothingUsable(sri.to_owned()).into())
}

impl Checksum {
    /// `sha1` and `sha256` are hex. `sri` is a Subresource Integrity string, as npm lockfiles
    /// carry in `integrity`; see [`parse_sri`].
    pub fn new(
        sha1: Option<&str>,
        sha256: Option<&str>,
        sri: Option<&str>,
    ) -> buck2_error::Result<Self> {
        fn is_hex_digit(x: char) -> bool {
            let x = x.to_ascii_lowercase();
            x.is_ascii_digit() || ('a'..='f').contains(&x)
        }

        fn validate_digest(
            digest: Option<&str>,
            digest_len: usize,
            digest_type: &'static str,
        ) -> buck2_error::Result<Option<Arc<str>>> {
            match digest {
                None => Ok(None),
                Some(digest) => {
                    let expected_len = digest_len * 2;
                    if digest.len() != expected_len {
                        return Err(DownloadFileError::InvalidDigestLength {
                            digest: digest.to_owned(),
                            expected_len,
                            digest_type,
                        }
                        .into());
                    }
                    if let Some(bad_char) = digest.chars().find(|x| !is_hex_digit(*x)) {
                        return Err(DownloadFileError::InvalidDigestCharacter {
                            digest: digest.to_owned(),
                            bad_char,
                            digest_type,
                        }
                        .into());
                    }
                    Ok(Some(Arc::from(digest.to_ascii_lowercase())))
                }
            }
        }

        let checksum = Checksum {
            sha1: validate_digest(sha1, SHA1_SIZE, "sha1")?,
            sha256: validate_digest(sha256, SHA256_SIZE, "sha256")?,
            sri: sri.map(parse_sri).transpose()?.map(Arc::new),
        };
        if checksum.sha1.is_none() && checksum.sha256.is_none() && checksum.sri.is_none() {
            return Err(DownloadFileError::MissingChecksum.into());
        }
        Ok(checksum)
    }

    /// The one SHA-1 the content must have, if there is one: the `sha1` argument, or else an
    /// `sri` whose strongest hashes are a single SHA-1.
    pub fn sha1(&self) -> Option<&str> {
        self.sha1
            .as_deref()
            .or_else(|| self.sri_single(SriAlgorithm::Sha1))
    }

    /// As [`Self::sha1`], for SHA-256.
    pub fn sha256(&self) -> Option<&str> {
        self.sha256
            .as_deref()
            .or_else(|| self.sri_single(SriAlgorithm::Sha256))
    }

    fn sri_single(&self, algorithm: SriAlgorithm) -> Option<&str> {
        match self.sri.as_deref() {
            Some(SriChecksum {
                algorithm: a,
                digests,
            }) if *a == algorithm => match &**digests {
                [digest] => Some(digest),
                _ => None,
            },
            _ => None,
        }
    }
}

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Http)]
enum HttpHeadError {
    #[error("Error performing http_head request")]
    Client(#[source] HttpError),
}

impl From<HttpError> for HttpHeadError {
    fn from(e: HttpError) -> Self {
        Self::Client(e)
    }
}

#[derive(Debug, buck2_error::Error)]
#[buck2(tag = Http)]
enum HttpDownloadError {
    #[error("Error performing http_download request")]
    Client(#[source] HttpError),

    #[error(
        "Invalid {digest_kind} digest. Expected {expected}, got {obtained}. URL: {url}. {debug}"
    )]
    #[buck2(input)]
    InvalidChecksum {
        digest_kind: &'static str,
        expected: String,
        obtained: String,
        url: String,
        debug: Box<MaybeResponseDebugInfo>,
    },

    #[error(
        "Received invalid {kind} digest from {url}; perhaps this is not allowed on vpnless?. Expected {want}, got {got}. {debug}"
    )]
    MaybeNotAllowedOnVpnless {
        kind: &'static str,
        want: String,
        got: String,
        url: String,
        debug: Box<MaybeResponseDebugInfo>,
    },

    #[error(transparent)]
    IoError(buck2_error::Error),
}

impl HttpDownloadError {
    fn into_final(mut self) -> Self {
        match &mut self {
            Self::Client(..) | Self::IoError(..) => {}
            Self::InvalidChecksum { debug, .. } | Self::MaybeNotAllowedOnVpnless { debug, .. } => {
                debug.is_final = true;
            }
        }

        self
    }
}

impl From<HttpError> for HttpDownloadError {
    fn from(e: HttpError) -> Self {
        Self::Client(e)
    }
}

impl HttpErrorForRetry for HttpHeadError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Client(e) => e.is_retryable(),
        }
    }
}

impl HttpErrorForRetry for HttpDownloadError {
    fn is_retryable(&self) -> bool {
        match self {
            Self::Client(e) => e.is_retryable(),
            Self::InvalidChecksum { .. } => {
                // Normally, invalid checksums don't make sense to retry, but the HTTP servers we
                // talk to internally tend to happily return 200s and give you the error in the
                // message body... so it's a good idea to retry those.
                cfg!(fbcode_build)
            }
            Self::IoError(..) | Self::MaybeNotAllowedOnVpnless { .. } => false,
        }
    }
}

impl IntoBuck2Error for HttpHeadError {
    fn into_buck2_error(self) -> buck2_error::Error {
        buck2_error::Error::from(self)
    }
}

impl IntoBuck2Error for HttpDownloadError {
    fn into_buck2_error(self) -> buck2_error::Error {
        buck2_error::Error::from(self)
    }
}

pub async fn http_head(client: &HttpClient, url: &str) -> buck2_error::Result<Response<()>> {
    let response = http_retry(
        || async {
            client
                .head(url)
                .await
                .map_err(|e| HttpHeadError::Client(HttpError::Client(e)))
        },
        vec![2, 4, 8].into_iter().map(Duration::from_secs).collect(),
    )
    .await?;
    Ok(response)
}

pub async fn http_download(
    client: &HttpClient,
    fs: &ProjectRoot,
    digest_config: DigestConfig,
    path: &ProjectRelativePath,
    url: &str,
    checksum: &Checksum,
    executable: bool,
) -> buck2_error::Result<TrackedFileDigest> {
    let abs_path = fs.resolve(path);
    if let Some(dir) = abs_path.parent() {
        fs_util::create_dir_all(dir)?;
    }

    Ok(http_retry(
        || async {
            let response = client
                .get(url)
                .await
                .map_err(|e| HttpDownloadError::Client(HttpError::Client(e)))?;

            let (head, stream) = response.into_parts();
            let file = fs_util::create_file(&abs_path)
                .categorize_internal()
                .map_err(HttpDownloadError::IoError)?;
            let buf_writer = std::io::BufWriter::new(file);

            let digest = copy_and_hash(
                url,
                Some(head),
                &abs_path,
                stream,
                buf_writer,
                digest_config.cas_digest_config(),
                checksum,
                client.supports_vpnless(),
            )
            .await?;

            if executable {
                fs.set_executable(path)
                    .map_err(HttpDownloadError::IoError)?;
            }

            Result::<_, HttpDownloadError>::Ok(TrackedFileDigest::new(
                digest,
                digest_config.cas_digest_config(),
            ))
        },
        vec![2, 4, 8].into_iter().map(Duration::from_secs).collect(),
    )
    .await
    .map_err(|e| e.into_final())?)
}

/// Copy a stream into a writer while producing its digest and checksumming it.
async fn copy_and_hash(
    url: &str,
    head: Option<http::response::Parts>,
    abs_path: &(impl std::fmt::Display + ?Sized),
    mut stream: impl Stream<Item = Result<Bytes, hyper::Error>> + Unpin,
    mut writer: impl Write,
    digest_config: CasDigestConfig,
    checksum: &Checksum,
    is_vpnless: bool,
) -> Result<FileDigest, HttpDownloadError> {
    let mut digester = FileDigest::digester(digest_config);

    // For each checksum entry we have, we're going to add a validator. We might have to create
    // a new hasher, or reuse the `FileDigest::digester` if it matches.

    enum Validator {
        PrimaryDigest,
        ExtraDigest(Box<dyn DynDigest + Send>),
    }

    // Reuse the `FileDigest::digester` when it computes the same hash.
    let validator = |algorithm: SriAlgorithm| match algorithm.digest_family() {
        Some(family) if family == digester.algorithm() => Validator::PrimaryDigest,
        _ => Validator::ExtraDigest(algorithm.hasher()),
    };

    // Each entry passes if the content's digest is any of `expected`. Only an `sri` gives more
    // than one; its digests are shown in SRI form, the form they were given in.
    let mut validators = SmallVec::<[(Validator, &[Arc<str>], &'static str, bool); 3]>::new();
    if let Some(sha1) = &checksum.sha1 {
        validators.push((
            validator(SriAlgorithm::Sha1),
            std::slice::from_ref(sha1),
            "sha1",
            false,
        ));
    }
    if let Some(sha256) = &checksum.sha256 {
        validators.push((
            validator(SriAlgorithm::Sha256),
            std::slice::from_ref(sha256),
            "sha256",
            false,
        ));
    }
    if let Some(sri) = checksum.sri.as_deref() {
        validators.push((
            validator(sri.algorithm),
            &sri.digests,
            sri.algorithm.name(),
            true,
        ));
    }

    let mut buff = DebugBuffer::new(512);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| HttpError::Transfer {
            received: digester.bytes_read(),
            url: url.to_owned(),
            source,
        })?;

        buff.peek(&chunk);

        writer
            .write_all(&chunk)
            .with_buck_error_context(|| format!("write({abs_path})"))
            .map_err(HttpDownloadError::IoError)?;

        digester.update(&chunk);
        for (validator, ..) in validators.iter_mut() {
            if let Validator::ExtraDigest(hasher) = validator {
                hasher.update(&chunk);
            }
        }
    }
    writer
        .flush()
        .with_buck_error_context(|| format!("flush({abs_path})"))
        .map_err(HttpDownloadError::IoError)?;

    let digest = digester.finalize();

    // Validate
    for (validator, expected, kind, sri) in validators {
        let obtained = match validator {
            Validator::PrimaryDigest => digest.raw_digest().to_string(),
            Validator::ExtraDigest(hasher) => hex::encode(hasher.finalize()),
        };

        if !expected.iter().any(|e| **e == *obtained) {
            let show = |hex_digest: &str| {
                if sri {
                    let bytes = hex::decode(hex_digest).unwrap_or_default();
                    format!(
                        "{kind}-{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    )
                } else {
                    hex_digest.to_owned()
                }
            };
            let expected = expected
                .iter()
                .map(|e| show(e))
                .collect::<Vec<_>>()
                .join(" or ");
            let obtained = show(&obtained);
            let debug = Box::new(MaybeResponseDebugInfo {
                bytes_seen: buff.bytes_seen,
                buff: buff.to_utf8().map(ToOwned::to_owned),
                head,
                is_final: false,
            });

            if is_vpnless {
                return Err(HttpDownloadError::MaybeNotAllowedOnVpnless {
                    kind,
                    want: expected,
                    got: obtained,
                    url: url.to_owned(),
                    debug,
                });
            }
            return Err(HttpDownloadError::InvalidChecksum {
                digest_kind: kind,
                expected,
                obtained,
                url: url.to_owned(),
                debug,
            });
        }
    }

    Ok(digest)
}

struct DebugBuffer {
    bytes_seen: u64,
    max_size: usize,
    buff: Vec<u8>,
}

impl DebugBuffer {
    fn new(max_size: usize) -> Self {
        Self {
            bytes_seen: 0,
            max_size,
            buff: Vec::new(),
        }
    }

    fn peek(&mut self, chunk: &[u8]) {
        // unwrap safety: we can't possibly have a chunk whose length can't be represented with 64
        // bits.
        self.bytes_seen += u64::try_from(chunk.len()).unwrap();

        if self.buff.len() < self.max_size {
            let want_bytes = std::cmp::min(chunk.len(), self.max_size - self.buff.len());
            self.buff.extend(&chunk[..want_bytes]);
        }
    }

    fn to_utf8(&self) -> Option<&str> {
        match std::str::from_utf8(&self.buff) {
            Ok(utf8) => Some(utf8),
            Err(e) => {
                let valid_up_to = e.valid_up_to();

                // If at least 50% of the buffer is valid UTF8, let's show it.
                if valid_up_to >= (self.max_size / 2) {
                    std::str::from_utf8(&self.buff[..valid_up_to]).ok()
                } else {
                    None
                }
            }
        }
    }
}

/// A little helper to avoid showing the debug data in http_retry because that's reall verbose.
#[derive(Debug)]
struct MaybeResponseDebugInfo {
    bytes_seen: u64,
    buff: Option<String>,
    head: Option<http::response::Parts>,
    is_final: bool,
}

impl fmt::Display for MaybeResponseDebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Received {} bytes.", self.bytes_seen)?;

        if !self.is_final {
            return Ok(());
        }

        if let Some(head) = &self.head {
            let interesting_headers = ["error-mid", "x-fb-debug"]
                .into_iter()
                .filter_map(|header| {
                    head.headers
                        .get(header)
                        .and_then(|h| h.to_str().ok())
                        .map(|header_value| (header, header_value))
                })
                .collect::<Vec<_>>();

            write!(f, "\n\nRelevant debug headers:\n")?;
            if interesting_headers.is_empty() {
                write!(f, "<none>")?;
            } else {
                for (header, header_value) in interesting_headers {
                    writeln!(f, "{header}: {header_value}")?;
                }
            }
        }

        match &self.buff {
            Some(text) => {
                write!(f, "\n\nResponse started with:\n\n{text}")?;
            }
            None => {
                write!(f, "Response is not UTF-8")?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use buck2_common::cas_digest::testing;
    use futures::stream;

    use super::*;

    // SRI hashes of `foobar`, the content `do_test` streams, and of the empty string.
    const FOOBAR_SHA1: &str = "sha1-iEPX+SQWIR3p67lj/0zigSWTKHg=";
    const FOOBAR_SHA256: &str = "sha256-w6uP8Tcg6K2QR905Rms8iXTlksL6OD1KOWBxTK7wxPI=";
    const FOOBAR_SHA384: &str =
        "sha384-PJww2fZl501RXIQpYNSkUcg6ASX9Pec5LXs3IxrxDHLqWK7fzfiaV2W/kCr5Ps8G";
    const FOOBAR_SHA512: &str = "sha512-ClAmHr0aOQ/tK/Mm8mc8FFWCpjQtUjIElz0CGTN/gWFqgGmwElh89WNfaSXxtWw2AjDBmyc1AO4BPgMGAb8kJQ==";
    const EMPTY_SHA256: &str = "sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
    const EMPTY_SHA512: &str = "sha512-z4PhNX7vuL3xVChQ1m2AB9Yg5AULVxXcg/SpIdNs6c5H0NE8XYXysP+DGNKHfuwvY7kxvUdBeoGlODJ6+SfaPg==";

    // Unvalidated, so a test can expect a digest that is not even hex.
    fn checksum_sha1(sha1: &str) -> Checksum {
        Checksum {
            sha1: Some(Arc::from(sha1)),
            sha256: None,
            sri: None,
        }
    }

    fn checksum_sha256(sha256: &str) -> Checksum {
        Checksum {
            sha1: None,
            sha256: Some(Arc::from(sha256)),
            sri: None,
        }
    }

    async fn do_test(
        digest_config: CasDigestConfig,
        checksum: &Checksum,
    ) -> Result<(FileDigest, Vec<u8>), HttpDownloadError> {
        let mut out = Vec::new();

        let digest = copy_and_hash(
            "test",
            None,
            "test",
            stream::iter(vec![Ok(Bytes::from("foo")), Ok(Bytes::from("bar"))]),
            &mut out,
            digest_config,
            checksum,
            false,
        )
        .await?;

        Ok((digest, out))
    }

    #[tokio::test]
    async fn test_copy_and_hash_ok() -> buck2_error::Result<()> {
        let (digest, bytes) = do_test(
            testing::blake3(),
            &Checksum::new(
                Some("8843d7f92416211de9ebb963ff4ce28125932878"),
                Some("c3ab8ff13720e8ad9047dd39466b3c8974e592c2fa383d4a3960714caef0c4f2"),
                Some(FOOBAR_SHA512),
            )?,
        )
        .await?;

        assert_eq!(
            digest.to_string(),
            "aa51dcd43d5c6c5203ee16906fd6b35db298b9b2e1de3fce81811d4806b76b7d:6"
        );

        assert_eq!(std::str::from_utf8(&bytes).unwrap(), "foobar");

        Ok(())
    }
    /// `copy_and_hash` writes every byte of a chunk even when the writer accepts a chunk in
    /// several pieces.
    #[tokio::test]
    async fn test_copy_and_hash_short_write() -> buck2_error::Result<()> {
        struct ShortWriter<'a>(&'a mut Vec<u8>);
        impl Write for ShortWriter<'_> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let n = buf.len().min(2);
                self.0.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut out = Vec::new();
        let digest = copy_and_hash(
            "test",
            None,
            "test",
            stream::iter(vec![Ok(Bytes::from("foo")), Ok(Bytes::from("bar"))]),
            ShortWriter(&mut out),
            testing::sha1(),
            &checksum_sha1("8843d7f92416211de9ebb963ff4ce28125932878"),
            false,
        )
        .await?;

        assert_eq!(digest.size(), 6);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "foobar");
        Ok(())
    }

    #[tokio::test]
    async fn test_copy_and_hash_invalid_primary_hash() -> buck2_error::Result<()> {
        assert_matches!(
            do_test(testing::sha1(), &checksum_sha1("oops")).await,
            Err(HttpDownloadError::InvalidChecksum { .. })
        );

        assert_matches!(
            do_test(testing::sha256(), &checksum_sha256("oops")).await,
            Err(HttpDownloadError::InvalidChecksum { .. })
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_copy_and_hash_invalid_secondary_hash() -> buck2_error::Result<()> {
        assert_matches!(
            do_test(testing::blake3(), &checksum_sha1("oops")).await,
            Err(HttpDownloadError::InvalidChecksum { .. })
        );

        assert_matches!(
            do_test(testing::blake3(), &checksum_sha256("oops")).await,
            Err(HttpDownloadError::InvalidChecksum { .. })
        );

        Ok(())
    }

    fn sri(sri: &str) -> Checksum {
        Checksum::new(None, None, Some(sri)).unwrap()
    }

    async fn sri_passes(sri_str: &str) -> bool {
        match do_test(testing::sha1(), &sri(sri_str)).await {
            Ok(_) => true,
            Err(HttpDownloadError::InvalidChecksum { .. }) => false,
            Err(e) => panic!("{e}"),
        }
    }

    #[tokio::test]
    async fn test_copy_and_hash_sri() {
        // Each algorithm on its own.
        assert!(sri_passes(FOOBAR_SHA512).await);
        assert!(sri_passes(FOOBAR_SHA384).await);
        assert!(sri_passes(FOOBAR_SHA256).await);
        assert!(sri_passes(FOOBAR_SHA1).await);
        assert!(!sri_passes(EMPTY_SHA512).await);

        // Only the strongest algorithm counts: a wrong sha256 beside a right sha512 is not
        // checked, and a right sha256 beside a wrong sha512 does not help.
        assert!(sri_passes(&format!("{EMPTY_SHA256} {FOOBAR_SHA512}")).await);
        assert!(!sri_passes(&format!("{FOOBAR_SHA256} {EMPTY_SHA512}")).await);
        assert!(sri_passes(&format!("{EMPTY_SHA256}\t{FOOBAR_SHA384}")).await);
        assert!(!sri_passes(&format!("{FOOBAR_SHA384}\n{EMPTY_SHA512}")).await);

        // Any digest of the strongest algorithm may match.
        assert!(sri_passes(&format!("{EMPTY_SHA512} {FOOBAR_SHA512}")).await);
        assert!(sri_passes(&format!("{FOOBAR_SHA512} {EMPTY_SHA512}")).await);

        // Options are ignored, and so are tokens of unknown algorithms or no algorithm at all.
        assert!(sri_passes(&format!("{FOOBAR_SHA512}?foo=bar")).await);
        assert!(sri_passes(&format!("md5-AAAA {FOOBAR_SHA256} junk")).await);
        assert!(!sri_passes(&format!("{EMPTY_SHA256} sha1024-AAAA")).await);
        // Algorithm names are not case-sensitive.
        assert!(sri_passes(&FOOBAR_SHA512.replacen("sha512", "SHA512", 1)).await);
    }

    #[tokio::test]
    async fn test_copy_and_hash_sri_mismatch_is_reported_in_sri_form() {
        let e = do_test(
            testing::sha1(),
            &sri(&format!("{EMPTY_SHA512} {EMPTY_SHA512}")),
        )
        .await
        .unwrap_err();
        assert_matches!(
            &e,
            HttpDownloadError::InvalidChecksum { digest_kind: "sha512", expected, obtained, .. }
                if *expected == format!("{EMPTY_SHA512} or {EMPTY_SHA512}") && obtained == FOOBAR_SHA512
        );
    }

    #[test]
    fn test_sri_single_digest_names_the_content() -> buck2_error::Result<()> {
        // A single sha1 or sha256 is as good as the hex argument for naming the content.
        assert_eq!(
            sri(FOOBAR_SHA1).sha1(),
            Some("8843d7f92416211de9ebb963ff4ce28125932878")
        );
        assert_eq!(
            sri(FOOBAR_SHA256).sha256(),
            Some("c3ab8ff13720e8ad9047dd39466b3c8974e592c2fa383d4a3960714caef0c4f2")
        );
        // Not when it is outranked, or when it is one of several.
        assert_eq!(sri(&format!("{FOOBAR_SHA1} {FOOBAR_SHA512}")).sha1(), None);
        assert_eq!(
            sri(&format!("{FOOBAR_SHA256} {EMPTY_SHA256}")).sha256(),
            None
        );
        Ok(())
    }

    #[test]
    fn test_sri_rejects() {
        for (sri, wanted) in [
            ("", "No usable hash"),
            ("   ", "No usable hash"),
            ("sha512", "No usable hash"),
            ("md5-rL0Y20zC+Fzt72VPzMSk2A==", "No usable hash"),
            ("sha512-not*base64", "a sha512 digest is 64 bytes of base64"),
            (
                FOOBAR_SHA1.replacen("sha1", "sha512", 1).leak(),
                "a sha512 digest is 64 bytes of base64",
            ),
        ] {
            let e = Checksum::new(None, None, Some(sri)).expect_err(sri);
            let e = format!("{e:#}");
            assert!(e.contains(wanted), "`{sri}` gave `{e}`, wanted `{wanted}`");
        }
        let e = Checksum::new(None, None, None).unwrap_err();
        assert!(format!("{e:#}").contains("at least one checksum"), "{e:#}");
    }

    #[test]
    fn test_debug_buffer() {
        let mut buff = DebugBuffer::new(10);
        buff.peek(b"foo");
        assert_eq!(buff.to_utf8(), Some("foo"));

        let mut buff = DebugBuffer::new(2);
        buff.peek(b"foo");
        assert_eq!(buff.to_utf8(), Some("fo"));

        let mut buff = DebugBuffer::new(4);
        buff.peek(b"foo");
        buff.peek(b"foo");
        assert_eq!(buff.to_utf8(), Some("foof"));

        // 75% fine.
        let mut buff = DebugBuffer::new(4);
        buff.peek(b"foo");
        buff.peek(&[0xff]);
        assert_eq!(buff.to_utf8(), Some("foo"));

        // 50% fine.
        let mut buff = DebugBuffer::new(4);
        buff.peek(b"fo");
        buff.peek(&[0xff]);
        assert_eq!(buff.to_utf8(), Some("fo"));

        // 25% fine.
        let mut buff = DebugBuffer::new(4);
        buff.peek(b"f");
        buff.peek(&[0xff]);
        assert_eq!(buff.to_utf8(), None);

        // 0% fine.
        let mut buff = DebugBuffer::new(4);
        buff.peek(&[0xff]);
        assert_eq!(buff.to_utf8(), None);
    }
}
