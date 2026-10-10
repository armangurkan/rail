/// Whether an operation is worth retrying.
///
/// Workers retry `Transient` failures with backoff and never retry `Permanent` ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Network trouble, 5xx, 429: try again later.
    Transient,
    /// 4xx, parse failure, revoked auth: retrying cannot help.
    Permanent,
}
