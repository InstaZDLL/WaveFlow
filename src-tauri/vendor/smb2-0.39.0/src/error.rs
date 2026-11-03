//! Error types for the SMB2 library.

use crate::types::status::NtStatus;
use crate::types::Command;
use thiserror::Error;

/// Why a durable handle could not be claimed back.
///
/// All four mean "reopen and rewrite from the start" to a caller. They are
/// told apart because only one of them says anything is wrong with the server:
/// [`IdentityMismatch`](Self::IdentityMismatch) means it matched our
/// `CreateGuid` and still handed back a different file, which the protocol
/// says cannot happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DurableLoss {
    /// The server no longer has the open: it timed out, or the server
    /// restarted (a durable handle survives a dead connection, not a dead
    /// server — only a *persistent* handle on a continuously-available share
    /// does that). Routine.
    Expired,
    /// The server gave the handle back but would not say which file it points
    /// at, so nothing could be proven. The handle was closed again.
    IdentityUnavailable,
    /// The server gave back a handle to a **different file**. The handle was
    /// closed and the write refused; writing into it is the outcome this whole
    /// mechanism exists to prevent.
    IdentityMismatch,
    /// The handle was never durable, so there was nothing to reclaim.
    NotDurable,
}

impl std::fmt::Display for DurableLoss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Expired => "the server no longer has the open",
            Self::IdentityUnavailable => "the server would not say which file the handle points at",
            Self::IdentityMismatch => "the server handed back a different file",
            Self::NotDurable => "the handle was never durable",
        };
        f.write_str(s)
    }
}

/// Why an interrupted upload can't carry on from its checkpoint.
///
/// All four mean "write the file again from the start" to a caller. They are
/// told apart so a consumer can say what happened: only
/// [`Replaced`](Self::Replaced) and [`Truncated`](Self::Truncated) mean
/// someone else changed the file, and
/// [`IdentityUnavailable`](Self::IdentityUnavailable) means this server can
/// never resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResumeRefusal {
    /// Nothing is at the path any more. The resume never creates a file, since
    /// a new one would start with a hole where the checkpointed bytes were.
    Missing,
    /// A file is at the path, but not the one the checkpoint names: it was
    /// deleted and recreated, or another file was renamed over it.
    Replaced,
    /// Nothing can prove the file is the one the checkpoint names, because
    /// the server didn't identify it in full, either now or when the
    /// checkpoint was made (see
    /// [`WriteCheckpoint::identity`](crate::WriteCheckpoint::identity)).
    IdentityUnavailable,
    /// The file is shorter than the checkpoint, so bytes the checkpoint
    /// vouches for are gone: someone truncated it, or the server lost what it
    /// had flushed.
    Truncated {
        /// The file's length when the resume opened it.
        length: u64,
    },
}

impl std::fmt::Display for ResumeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("the file is gone"),
            Self::Replaced => f.write_str("a different file is at that path now"),
            Self::IdentityUnavailable => {
                f.write_str("the server won't say which file it is, so it can't be proven the same")
            }
            Self::Truncated { length } => write!(f, "the file is only {length} bytes now"),
        }
    }
}

/// What was wrong with a response's signature.
///
/// Both mean the response was refused unread: it can't be shown to come from
/// the server that holds the session key, so neither its status nor its
/// payload can be trusted. They're told apart for the person diagnosing it. A
/// [`Missing`](Self::Missing) signature most often means a server, or
/// something on the path that strips signatures, sending responses unsigned
/// on a session that has to sign them. A [`Mismatch`](Self::Mismatch) means
/// the bytes were altered on the way, or the server derived a different key.
/// Either can be an attack, which is why neither is retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignatureFailure {
    /// The response carries no signature: its signature field is empty
    /// (all zeros), or, for the final SESSION_SETUP response on SMB 3.x, the
    /// server didn't flag it signed. Never decided by the
    /// `SMB2_FLAGS_SIGNED` bit alone on any other response: that bit travels
    /// in the same untrusted bytes as the payload.
    Missing,
    /// The response carries a signature, and it isn't the one the session
    /// key produces for these bytes.
    Mismatch,
}

impl std::fmt::Display for SignatureFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Missing => "isn't signed",
            Self::Mismatch => "has a signature that doesn't match",
        };
        f.write_str(s)
    }
}

/// Top-level error type for SMB2 operations.
///
/// `#[non_exhaustive]`: new variants appear as the crate learns to tell more
/// failures apart, so `match` on it with a `_` arm, or branch on
/// [`Error::kind`] instead. Adding a variant is not treated as a breaking
/// change.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The server answered a stream's delete with success and kept the stream.
    /// A QNAP does this for `com.apple.metadata:_kMDItemUserTags` and
    /// `AFP_Resource`, to macOS's own client too. For Finder tags, write what
    /// macOS writes to clear them: a plist holding an empty array. Kind
    /// [`ErrorKind::Unsupported`]. Not retryable.
    #[error("the server kept stream {stream:?} of {path:?} after reporting it deleted")]
    StreamDeleteIgnored {
        /// The path, as the caller gave it.
        path: String,
        /// The stream the server kept.
        stream: String,
    },

    /// The server refused a stream's name: this share has no named streams. A
    /// plain Samba with no `streams_*` module answers this way, with
    /// `STATUS_OBJECT_NAME_INVALID`. Kind [`ErrorKind::Unsupported`]. Not
    /// retryable.
    #[error("this share has no named streams (path {path:?})")]
    NamedStreamsUnsupported {
        /// The path whose stream the server refused, as the caller gave it.
        path: String,
    },

    /// The data is malformed or does not match the expected format.
    #[error("Invalid data: {message}")]
    InvalidData {
        /// Description of what went wrong.
        message: String,
    },

    /// The server returned a non-success NTSTATUS.
    #[error("Protocol error: {status} during {command:?}")]
    Protocol {
        /// The NTSTATUS code from the response header.
        status: NtStatus,
        /// The command that triggered the error.
        command: Command,
    },

    /// Authentication failed.
    #[error("Authentication failed: {message}")]
    Auth {
        /// Description of what went wrong.
        message: String,
    },

    /// A response on a session that signs failed its signature check, so it
    /// was refused unread.
    ///
    /// On a signed session every response has to verify against the session
    /// key, except the two MS-SMB2 § 3.2.5.1.3 exempts (an interim
    /// `STATUS_PENDING` and an unsolicited oplock break). One that doesn't is
    /// either a broken server or someone on the path rewriting it, and
    /// nothing in it, its status included, can be believed. `reason` says
    /// whether the signature was absent or wrong.
    ///
    /// Two places return it:
    ///
    /// - **Any response on an established signed session**: the request it
    ///   answers fails with this, and the connection stays up. Encrypted
    ///   responses are authenticated by their encryption instead and never
    ///   reach this check.
    /// - **The final SESSION_SETUP response of a login that names an
    ///   account**, which decides whether the session signs or encrypts at
    ///   all, so it has to prove itself before its flags are believed. The
    ///   login fails with this, `command` is
    ///   [`SessionSetup`](Command::SessionSetup), and no session is set up.
    ///
    /// Classifies as [`ErrorKind::SignatureInvalid`] and is **not**
    /// retryable: sending the same request again over the same path invites
    /// the same answer. Surface it to a person as a security problem with the
    /// connection, not as a missing file or a wrong password. Every refusal
    /// also counts in the connection's `signature_failures` diagnostic,
    /// except the SESSION_SETUP one, which happens before the session exists.
    #[error("the server's {command} response (message {}) {reason}, so it was refused", message_id.0)]
    SignatureInvalid {
        /// The command the refused response answers.
        command: Command,
        /// The response's `MessageId`, for matching it against a trace.
        message_id: crate::types::MessageId,
        /// Whether the signature was absent or wrong.
        reason: SignatureFailure,
    },

    /// A login that named an account was answered with a guest or anonymous
    /// session, and the client refused it.
    ///
    /// The caller asked to be someone, and a guest or anonymous session is
    /// unsigned, so taking it would quietly drop both the identity and the
    /// protection. Samba's `map to guest = bad user` answers a wrong password
    /// or an unknown user this way, which makes that the common cause; an
    /// on-path attacker setting `IS_GUEST` in the final SESSION_SETUP
    /// response looks exactly the same (MS-SMB2 § 3.2.5.3.1 refuses it for
    /// the same reason). A login with an empty username, or with the name
    /// `Guest` in any ASCII case, asks for a guest session and never gets
    /// this.
    ///
    /// Classifies as [`ErrorKind::GuestSessionRefused`] and is not retryable:
    /// check the username and password, or log in without a username to take
    /// the guest session on purpose.
    #[error(
        "the server offered {} instead of signing in as {username}. Check the username and \
         password: a Samba server set to `map to guest = bad user` answers a wrong password \
         this way. To sign in as guest, leave the username empty or use `Guest`",
        if *anonymous { "an anonymous session" } else { "a guest session" }
    )]
    GuestSessionRefused {
        /// The account the login named.
        username: String,
        /// `true` when the server offered an anonymous (null) session,
        /// `false` when it offered a guest one.
        anonymous: bool,
    },

    /// An I/O or transport error occurred.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The operation timed out.
    #[error("Operation timed out")]
    Timeout,

    /// Every address a name resolved to failed.
    ///
    /// [`Error::Timeout`] says a connect ran out of budget and nothing else.
    /// This says which addresses were tried and what each one did, which is
    /// the difference between "the server is down" and "three of its four
    /// addresses answer and the one your resolver returns first does not".
    ///
    /// Classifies as [`ErrorKind::ConnectionLost`] and reports as retryable.
    #[error("could not connect to {host}: {} address(es) tried", attempts.len())]
    ConnectFailed {
        /// The name, as the caller gave it.
        host: String,
        /// One entry per address, in the order they were attempted.
        attempts: Vec<crate::transport::ConnectAttempt>,
    },

    /// The connection was lost.
    #[error("Disconnected from server")]
    Disconnected,

    /// Bringing a dead connection back on a fresh socket did not work.
    ///
    /// Every attempt failed, or the whole revival ran past
    /// [`ReconnectPolicy::total_budget`](crate::client::connection::ReconnectPolicy::total_budget),
    /// or a revival failed recently enough that its verdict still stands (see
    /// [`ReconnectPolicy::failure_cooldown`](crate::client::connection::ReconnectPolicy::failure_cooldown)).
    ///
    /// The connection is dead and stays dead. Branch on `cause` to tell "the
    /// credentials are wrong now, ask the user" from "the network is down, try
    /// again later"; `reason` is display text for a human and ❌ must never be
    /// matched on. Classifies as [`ErrorKind::ConnectionLost`] and reports as
    /// retryable — the *connection* is finished, but the work usually is not,
    /// and re-running the file on a fresh client is the intended response.
    #[error("could not reconnect after {attempts} attempt(s) over {waited:?}: {reason}")]
    ReconnectFailed {
        /// Dials made before giving up.
        attempts: u32,
        /// Wall clock spent trying.
        waited: std::time::Duration,
        /// What the last attempt failed with, as a typed classification.
        cause: ErrorKind,
        /// The last failure rendered for a human. Display only.
        reason: String,
    },

    /// A durable handle could not be claimed back after a reconnect, so the
    /// interrupted transfer has to restart rather than resume.
    ///
    /// Never a data-safety failure: it is what the client returns *instead of*
    /// guessing. Branch on `reason` to tell an expired open (routine) from a
    /// server that handed back the wrong file (alarming, and logged at
    /// `error!`). Whatever the reason, the response is the same: reopen the
    /// file and write it from the start. Classifies as
    /// [`ErrorKind::ConnectionLost`] and reports as retryable.
    #[error("could not reclaim the durable handle for {path}: {reason}")]
    DurableHandleLost {
        /// The path the handle was opened at.
        path: String,
        /// Which guarantee did not hold.
        reason: DurableLoss,
    },

    /// An interrupted upload can't resume from its checkpoint, because the
    /// file at the path is no longer provably the one it was writing, or no
    /// longer holds what the checkpoint vouches for.
    ///
    /// Returned by
    /// [`SmbClient::resume_file_writer`](crate::SmbClient::resume_file_writer)
    /// before any byte is written, with the handle it opened already closed.
    /// The response is always the same: write the file again from the start.
    /// `reason` says why, for the person watching the transfer.
    ///
    /// Classifies as [`ErrorKind::ResumeRefused`] and is **not** retryable:
    /// asking again with the same checkpoint can only be refused again. A
    /// network failure during the resume comes back as that failure instead,
    /// so `is_retryable` is exactly "try the resume again later".
    #[error("can't resume the upload to {path} at offset {offset}: {reason}")]
    ResumeRefused {
        /// The path the resume opened.
        path: String,
        /// The checkpoint's offset.
        offset: u64,
        /// Which check failed.
        reason: ResumeRefusal,
    },

    /// A file handle or a [`Tree`](crate::Tree) outlived the session that
    /// opened it, or was used on another connection, so the request was
    /// refused before it reached the wire.
    ///
    /// A reconnect revives a [`Connection`](crate::client::connection::Connection)
    /// in place, so a handle that owns a clone of it ([`FileReader`](crate::client::stream::FileReader),
    /// [`FileWriter`](crate::FileWriter), [`FileDownload`](crate::FileDownload),
    /// [`FileUpload`](crate::FileUpload), [`Watcher`](crate::Watcher)) still
    /// holds a working connection afterwards. Its tree id and file id don't
    /// work, though: they belong to the session that died, and the new session
    /// numbers its own from scratch, so the same ids can name a different file
    /// or share there. Sending them would read or write the wrong file, so
    /// every request from such a handle fails with this instead. A `Tree` is
    /// the same: its tree id belongs to the session that connected it, and on
    /// a new session (or on another connection, like a DFS target's tree sent
    /// on the primary) the same id can be a different share.
    ///
    /// `opened_on` and `current` are
    /// [`Connection::generation`](crate::client::connection::Connection::generation)s.
    /// They're equal when a second `Session::setup` replaced the session on the
    /// same socket, without a reconnect.
    ///
    /// The handle is finished: open the file (or connect the share) again.
    /// `SmbClient`'s replaying calls (listings, reads, `stat`) do that for a
    /// stale tree themselves when `auto_reconnect` is on. Closing a handle sends nothing,
    /// since the server let go of it with the session. Classifies as
    /// [`ErrorKind::ConnectionLost`] and reports as retryable, like
    /// [`DurableHandleLost`](Self::DurableHandleLost): this handle can't be
    /// saved, and running the transfer again on a new one is the intended
    /// response.
    #[error(
        "this handle belongs to a session that has ended or to another connection \
         (opened on connection generation {opened_on}, now on {current}); open the file \
         or connect the share again"
    )]
    HandleInvalidated {
        /// The connection's generation when the handle was opened.
        opened_on: u64,
        /// The connection's generation when the request was refused.
        current: u64,
    },

    /// The path requires DFS referral resolution.
    ///
    /// The server returned `STATUS_PATH_NOT_COVERED`, meaning this path
    /// lives on a different server via DFS. The caller can query for a
    /// referral or display a helpful message.
    #[error("DFS referral required for path: {path}")]
    DfsReferralRequired {
        /// The path that needs DFS resolution.
        path: String,
    },

    /// The path is a DFS namespace, and none of its targets could be reached.
    ///
    /// Distinct from "the share does not exist", which is what a failed
    /// `TREE_CONNECT` normally means, and distinct from a referral that never
    /// came back — both of those surface as the original
    /// `STATUS_BAD_NETWORK_NAME`. This one says something no other error does:
    /// the namespace is real, we found it, the storage behind it is out of
    /// reach. A consumer's useful response is "the namespace is up, its file
    /// servers are not", which is a very different thing to tell someone than
    /// "no such share".
    ///
    /// Classifies as [`ErrorKind::ConnectionLost`] and reports as retryable:
    /// a target that is down now may be up later, and the namespace itself
    /// resolved fine.
    #[error("DFS namespace {namespace} resolved to {target_count} target(s), none reachable")]
    DfsNoReachableTarget {
        /// The namespace path the caller asked for, for example
        /// `\\corp.example.com\projects`.
        namespace: String,
        /// How many targets the referral offered.
        target_count: usize,
        /// Why the last one failed.
        source: Box<Error>,
    },

    /// A path sits behind a DFS link inside the share, and the link's target
    /// can't be reached.
    ///
    /// Says the link's other server is out of reach, ❌ never that the
    /// connection to the caller's share dropped, which is still fine. Other
    /// paths on the share keep working, so a consumer's useful response is to
    /// refuse this one path and leave its session alone. The next call through
    /// the link dials the target again.
    ///
    /// Two places return it: a call or
    /// [`SmbClient::resolve_dfs_link`](crate::SmbClient::resolve_dfs_link) that
    /// tried every target the referral named, and a call whose target
    /// connection dropped under it (`target_count` is 1, `source` says how). A
    /// link that points at the share's own server, on a connection that's gone,
    /// reports that loss instead: there it is the share's connection.
    ///
    /// A namespace root's targets being down is
    /// [`DfsNoReachableTarget`](Self::DfsNoReachableTarget): there the target
    /// IS the share.
    ///
    /// Classifies as [`ErrorKind::DfsTargetUnreachable`] and reports as
    /// retryable: a target that is down now may be up later.
    #[error("the DFS link behind {path} has {target_count} target(s), none reachable")]
    DfsTargetUnreachable {
        /// The path the call asked about, as the caller spelled it, relative
        /// to the tree the call was given.
        path: String,
        /// How many targets were tried.
        target_count: usize,
        /// Why the last one failed.
        source: Box<Error>,
    },

    /// A DFS referral points at a different server, and
    /// [`ClientConfig::dfs_target_policy`](crate::ClientConfig::dfs_target_policy)
    /// doesn't allow signing in there. Nothing went to that server.
    ///
    /// From a call through a link, it refuses that path alone: the share is fine.
    /// From [`connect_share`](crate::SmbClient::connect_share) on a namespace root,
    /// the target IS the share.
    ///
    /// Classifies as [`ErrorKind::DfsTargetNotAllowed`]. Not retryable: the answer
    /// only changes when the policy does.
    #[error("the DFS referral for {path} points at {target}, not {server}, and signing in there isn't allowed")]
    DfsTargetNotAllowed {
        /// The path or namespace the call asked about, as the caller spelled it.
        path: String,
        /// The host this client connected to ([`DfsSignIn::server`](crate::DfsSignIn::server)).
        server: String,
        /// The host the referral named ([`DfsSignIn::target`](crate::DfsSignIn::target)).
        target: String,
    },

    /// A DFS path kept referring us somewhere else, so we stopped following.
    ///
    /// A namespace that points at itself, or a chain of Interlinks with no end
    /// (MS-DFSC § 3.1.5.4.5), would otherwise resolve forever. The bound is a
    /// constant, not a setting: a legitimate namespace resolves in one hop and
    /// an Interlink adds one more, so anything near the limit is a
    /// misconfiguration on the server, and a knob would only let a consumer
    /// wait longer for the same answer.
    #[error("DFS path {namespace} was still being referred elsewhere after {hops} hops")]
    DfsTooManyReferrals {
        /// The path the caller asked for.
        namespace: String,
        /// How many referrals were followed before giving up.
        hops: usize,
    },

    /// The server moved this share to another node of a scale-out cluster.
    ///
    /// MS-SMB2 § 2.2.2.2.2: a `STATUS_BAD_NETWORK_NAME` carrying an
    /// `SMB2_ERROR_ID_SHARE_REDIRECT` error context. It reuses the status code
    /// a missing share uses, and means something else entirely — which is why
    /// it gets its own variant rather than a flag on
    /// [`Protocol`](Self::Protocol). Telling them apart also keeps DFS
    /// resolution off it: a cluster redirect is not a namespace, and chasing
    /// a referral for it would be nonsense.
    ///
    /// This crate implements neither side of the mechanism and never asks for
    /// it (it never sets `SMB2_TREE_CONNECT_FLAG_REDIRECT_TO_OWNER`), so a
    /// conforming server should not send it. Handling it anyway costs one
    /// branch and turns a silently-wrong answer into a true one.
    #[error("the server moved {share} to another cluster node, which this client does not follow")]
    ShareRedirected {
        /// The share the server refused, as the caller named it.
        share: String,
    },

    /// The operation was cancelled by the caller (via progress callback).
    #[error("Operation cancelled")]
    Cancelled,

    /// The session expired and reauthentication failed.
    ///
    /// The pipeline normally handles `STATUS_NETWORK_SESSION_EXPIRED`
    /// transparently by reauthenticating. This error surfaces only
    /// when reauthentication itself fails.
    #[error("Session expired and reauthentication failed")]
    SessionExpired,

    /// The file is larger than the single READ that was issued for it, so
    /// returning what came back would truncate it.
    ///
    /// Returned by [`Tree::read_file`](crate::Tree::read_file),
    /// [`Tree::read_file_compound`](crate::Tree::read_file_compound), and
    /// [`Tree::read_file_compound_sized`](crate::Tree::read_file_compound_sized).
    /// Those paths issue one READ, so a file bigger than that READ asked for
    /// can't come back whole; rather than silently dropping the tail, they fail
    /// with this. The two ways to hit it:
    ///
    /// - The file exceeds the server's negotiated per-READ maximum
    ///   (`MaxReadSize`), which is as small as 64 KiB on some servers.
    /// - The file outgrew the `expected_size` handed to
    ///   `read_file_compound_sized` between the caller's scan and the read.
    ///
    /// `size` is the server's authoritative size from the same round-trip.
    /// Retrying `read_file_compound_sized` with it fixes the second case only,
    /// where `size` still fits one READ. When `requested` is already the
    /// server's `MaxReadSize` the retry asks for the same bytes and fails the
    /// same way; [`Tree::read_file_pipelined`](crate::Tree::read_file_pipelined)
    /// reads any size in a sliding window of chunked READs and always works.
    /// Classifies as [`ErrorKind::TooLarge`].
    #[error(
        "file is {size} bytes, larger than the {requested}-byte single read \
         issued for it; retry with its real size if that fits one read, \
         otherwise use read_file_pipelined"
    )]
    FileTooLargeForSingleRead {
        /// The file's size in bytes, as the server reported it.
        size: u64,
        /// The number of bytes that single READ asked for, which is what
        /// bounds how much it could have returned. At most the server's
        /// `MaxReadSize`, and less when the caller supplied a smaller
        /// `expected_size`.
        requested: u32,
    },

    /// The server stopped granting credits, so the request could not be sent.
    ///
    /// Every SMB2 request spends credits from a budget the server grants and
    /// replenishes on each response. This crate never sends beyond that budget
    /// (doing so is a protocol violation the server may answer by dropping the
    /// connection, or — on some NAS firmware — by going silent). When the
    /// budget runs dry the send waits for a grant; this error says the wait
    /// ran out.
    ///
    /// In practice it means the server has stopped answering while the TCP
    /// connection is still up, so treat it as a dead connection: reconnect.
    /// Classifies as [`ErrorKind::TimedOut`] and reports as retryable.
    ///
    /// Tune the wait with
    /// [`Connection::set_credit_wait_timeout`](crate::client::connection::Connection::set_credit_wait_timeout).
    #[error(
        "server stopped granting SMB credits: needed {needed}, {available} available \
         after waiting {waited:?}"
    )]
    CreditStarvation {
        /// Credits the request needed (its `CreditCharge`).
        needed: u16,
        /// Credits on hand when the wait was abandoned.
        available: u16,
        /// How long the send waited for a grant.
        waited: std::time::Duration,
    },

    /// A request could not be handed to the network in time.
    ///
    /// This is the *send* side, not the response side: the bytes never
    /// reached the socket. A socket that stops accepting writes while TCP
    /// stays `ESTABLISHED` produces this, and so does a queue behind one
    /// such write.
    ///
    /// The distinction from [`Error::Timeout`] matters when reading logs. A
    /// `Timeout` means the server was asked and said nothing; a `SendTimeout`
    /// means the server was never asked, so nothing about the server can be
    /// inferred from it. A 2026-08-01 wedge was misread as server silence for
    /// exactly this reason: ~700 requests sat registered as in-flight with
    /// zero bytes on the wire.
    ///
    /// The connection is torn down when this fires: a write abandoned partway
    /// leaves half a frame on the wire, so the stream can't be trusted again.
    /// Classifies as [`ErrorKind::TimedOut`] and reports as retryable.
    ///
    /// Tune with
    /// [`Connection::set_send_timeout`](crate::client::connection::Connection::set_send_timeout).
    #[error("could not put a {bytes}-byte {command:?} request on the wire within {waited:?}")]
    SendTimeout {
        /// The command that was being sent (the first sub-op of a compound).
        command: crate::types::Command,
        /// Size of the frame that could not be written.
        bytes: usize,
        /// How long the send waited, queue time included.
        waited: std::time::Duration,
    },

    /// The socket failed while this request was being written to it, so the
    /// connection was torn down.
    ///
    /// The request's own send hit the failure: the server reset the
    /// connection (`ECONNRESET`), already closed it (`EPIPE` on the next
    /// write), or the OS gave up on it. `source` is that OS error, kept for
    /// the log and for a consumer that wants to say why; branch on
    /// [`Error::kind`], never on the io error. Whatever it is, the
    /// connection is gone: a write that fails partway leaves half a frame on
    /// the wire, so nothing sent after it could be trusted.
    ///
    /// Only the request whose send failed gets this. Every other request in
    /// flight on the connection gets [`Error::Disconnected`], and the
    /// connection already reports
    /// [`is_disconnected`](crate::client::connection::Connection::is_disconnected)
    /// by the time any of them hears. Classifies as
    /// [`ErrorKind::ConnectionLost`], the same as `Disconnected`, and reports
    /// as retryable. On an established connection, [`Error::Io`] never means
    /// the connection was lost.
    #[error("lost the connection while sending a {command:?} request: {source}")]
    SendFailed {
        /// The command that was being sent (the first sub-op of a compound).
        command: crate::types::Command,
        /// What the socket said.
        #[source]
        source: std::io::Error,
    },

    /// A request ran out of deadline on a connection the server had gone
    /// completely silent on, so the whole session was declared dead.
    ///
    /// This is [`Error::Timeout`] with a second fact attached. Both mean a
    /// request went unanswered for its full budget; this one adds that the
    /// server put *nothing* on the wire in the meantime, not even an answer to
    /// the SMB2 ECHO probes the keepalive sends (MS-SMB2 § 2.2.28, a request
    /// that touches no disk and no share). One stuck operation cannot look
    /// like that, so the connection is torn down and every other waiter is
    /// told at once rather than sitting out its own deadline one by one.
    ///
    /// The distinction from the other three is what it lets you conclude:
    ///
    /// - [`Error::Timeout`] -- *this* request went unanswered. The connection
    ///   may be perfectly healthy and the operation merely stuck; retrying it
    ///   on the same connection is reasonable.
    /// - [`Error::SendTimeout`] -- the request never reached the network, so
    ///   nothing at all follows about the server.
    /// - [`Error::Disconnected`] -- the socket itself went away (EOF, reset).
    /// - `ServerUnresponsive` -- the socket is up and the server is answering
    ///   nothing at all. Reconnect; retrying on this connection can only fail.
    ///
    /// Classifies as [`ErrorKind::ConnectionLost`] (the same as
    /// `Disconnected`, so existing reconnect paths pick it up unchanged) and
    /// reports as retryable. It cannot occur with the keepalive off
    /// ([`Connection::set_keepalive`](crate::client::connection::Connection::set_keepalive)),
    /// since nothing would then be asking: expect [`Error::Timeout`] instead.
    #[error("server stopped answering: nothing on the wire for {silent_for:?}")]
    ServerUnresponsive {
        /// How long since the server last put any frame on the wire.
        silent_for: std::time::Duration,
    },
}

impl Error {
    /// Create an `InvalidData` error with the given message.
    pub fn invalid_data(msg: impl Into<String>) -> Self {
        Error::InvalidData {
            message: msg.into(),
        }
    }

    /// Returns `true` if this error is potentially transient and
    /// the operation could succeed on retry.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Timeout
                | Error::Disconnected
                | Error::CreditStarvation { .. }
                | Error::SendTimeout { .. }
                | Error::SendFailed { .. }
                | Error::ServerUnresponsive { .. }
                | Error::ReconnectFailed { .. }
                | Error::DurableHandleLost { .. }
                | Error::HandleInvalidated { .. }
                | Error::DfsNoReachableTarget { .. }
                | Error::DfsTargetUnreachable { .. }
                | Error::ConnectFailed { .. }
                | Error::Protocol {
                    status: NtStatus::INSUFFICIENT_RESOURCES,
                    ..
                }
                | Error::Protocol {
                    status: NtStatus::INSUFF_SERVER_RESOURCES,
                    ..
                }
        )
    }

    /// Returns the NTSTATUS code if this is a protocol error.
    pub fn status(&self) -> Option<NtStatus> {
        match self {
            Error::Protocol { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// High-level error classification.
///
/// Maps protocol-level NTSTATUS codes and other errors into categories
/// that consumers can match on without understanding SMB internals.
///
/// ```no_run
/// # async fn example(client: &mut smb2::SmbClient, share: &mut smb2::Tree) -> Result<(), smb2::Error> {
/// use smb2::ErrorKind;
///
/// match client.read_file(share, "photo.jpg").await {
///     Ok(data) => println!("read {} bytes", data.len()),
///     Err(e) => match e.kind() {
///         ErrorKind::NotFound => println!("file doesn't exist"),
///         ErrorKind::AlreadyExists => println!("name is already taken"),
///         ErrorKind::AccessDenied => println!("no permission"),
///         ErrorKind::SigningRequired => println!("server requires signing, use credentials"),
///         ErrorKind::AuthRequired => println!("server requires authentication"),
///         ErrorKind::SharingViolation => println!("file is in use by another client"),
///         ErrorKind::IsADirectory => println!("path is a directory, not a file"),
///         ErrorKind::NotADirectory => println!("path is a file, not a directory"),
///         ErrorKind::DiskFull => println!("volume is full"),
///         ErrorKind::SignatureInvalid => println!("the reply was tampered with, or the server is broken"),
///         ErrorKind::ConnectionLost => { client.reconnect().await?; }
///         _ => return Err(e),
///     }
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Stability
///
/// `ErrorKind` is `#[non_exhaustive]`: future versions may add variants for
/// status codes that currently fall through to [`ErrorKind::Other`]. Match
/// statements should always include a `_` arm. Adding a variant is treated
/// as a non-breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The server requires authentication (guest/anonymous not allowed).
    AuthRequired,
    /// The server requires message signing (guest sessions are unsigned).
    SigningRequired,
    /// Permission denied (valid credentials, but no access to this resource).
    AccessDenied,
    /// The file, directory, or share was not found.
    NotFound,
    /// A file or directory with the given name already exists.
    ///
    /// Returned by `Create` (and operations that wrap it, like `create_directory`)
    /// when the target name is taken. Useful for callers that want to merge into
    /// an existing directory or surface a friendly "name already taken" message.
    AlreadyExists,
    /// The file is in use by another client.
    SharingViolation,
    /// The target path is a directory, but the operation expected a file.
    ///
    /// Typically seen when calling `delete_file` against a directory entry —
    /// the caller can fall back to `delete_directory` after detecting this.
    IsADirectory,
    /// The target path is a file, but the operation expected a directory.
    ///
    /// Typically seen when calling `list_directory` against a file entry.
    NotADirectory,
    /// The volume is full (write failed).
    DiskFull,
    /// The network connection was lost.
    ConnectionLost,
    /// The operation timed out.
    TimedOut,
    /// The operation was cancelled by the caller.
    Cancelled,
    /// The session expired (call `reconnect()`).
    SessionExpired,
    /// The path requires DFS referral resolution.
    DfsReferral,
    /// The path sits behind a DFS link whose target can't be reached. The
    /// connection to the share is fine, so ❌ don't reconnect on it: refuse this
    /// path and carry on. Only [`Error::DfsTargetUnreachable`] has it.
    /// Retryable.
    DfsTargetUnreachable,
    /// A DFS referral points at a different server, and the client's
    /// `dfs_target_policy` doesn't allow signing in there. Nothing went there.
    /// Only [`Error::DfsTargetNotAllowed`] has it. Not retryable.
    DfsTargetNotAllowed,
    /// Invalid data or malformed response.
    InvalidData,
    /// The file is too large for a single-read path.
    ///
    /// Returned by [`Tree::read_file`](crate::Tree::read_file) /
    /// [`read_file_compound`](crate::Tree::read_file_compound) /
    /// [`read_file_compound_sized`](crate::Tree::read_file_compound_sized)
    /// when the file is bigger than the READ they issued for it. Switch to
    /// [`read_file_pipelined`](crate::Tree::read_file_pipelined), which reads
    /// any size in chunked, pipelined READs.
    TooLarge,
    /// An I/O error (transport or callback). Not necessarily a connection loss.
    ///
    /// Distinct from `ConnectionLost`: the connection may still be usable.
    /// For example, a callback error in `write_file_streamed` produces `Io`,
    /// but the connection is still in a clean state.
    Io,
    /// The name is not usable on this server, whatever it is asked to do.
    ///
    /// Distinct from [`NotFound`](Self::NotFound): the file may or may not
    /// exist, and the server never got far enough to find out, so retrying the
    /// same name can only fail again. A consumer's useful response is to
    /// change the name, or tell the person that this one will not work here.
    ///
    /// The characters SMB2 forbids outright -- `"`, `*`, `:`, `<`, `>`, `?`,
    /// `\`, `|`, the control characters, and a trailing space or period -- are
    /// mapped out of the way automatically (see [`crate::name`]), so this is
    /// what is left over: a reserved Windows device name (`CON`, `NUL`,
    /// `LPT1`), a name past the server's own length limit, or a character its
    /// filesystem cannot store. Maps from `STATUS_OBJECT_NAME_INVALID`.
    InvalidName,
    /// The server does not support the requested operation.
    ///
    /// Returned when the server rejects an operation it does not implement --
    /// for example, an older Samba build or NAS firmware that lacks server-side
    /// copy. Consumers branch on this to fall back to a client-side path (for
    /// server-side copy, a plain read-then-write). Maps from
    /// `STATUS_NOT_SUPPORTED`, `STATUS_INVALID_DEVICE_REQUEST`, and
    /// `STATUS_NOT_IMPLEMENTED`, and from [`Error::NamedStreamsUnsupported`] and
    /// [`Error::StreamDeleteIgnored`].
    Unsupported,
    /// An interrupted upload can't resume from its checkpoint: write the
    /// file again from the start. Only [`Error::ResumeRefused`] has it, and
    /// its `reason` says why.
    ResumeRefused,
    /// A response on a signed session had a missing or wrong signature, so it
    /// was refused: a broken server, or the connection being tampered with.
    /// Only [`Error::SignatureInvalid`] has it, and its `reason` says which.
    /// Not retryable.
    SignatureInvalid,
    /// A login that named an account was answered with a guest or anonymous
    /// session, and the client refused it. Usually a wrong password on a
    /// server that maps failed logins to guest; check the credentials. Only
    /// [`Error::GuestSessionRefused`] has it. Not retryable.
    GuestSessionRefused,
    /// A protocol error not covered by other variants.
    ///
    /// Use [`Error::status()`] to get the raw NTSTATUS code. Some defined
    /// `NtStatus` codes deliberately fall through here today
    /// (`DELETE_PENDING`, `INSUFFICIENT_RESOURCES`, `INSUFF_SERVER_RESOURCES`,
    /// and similar) — they don't yet have a dedicated `ErrorKind` because no
    /// consumer needs to branch on them. Promoting one to its own variant is
    /// non-breaking, which is how `OBJECT_NAME_INVALID` became
    /// [`InvalidName`](Self::InvalidName).
    Other,
}

impl Error {
    /// Classify this error into a high-level category.
    ///
    /// Consumers can match on [`ErrorKind`] without understanding raw
    /// NTSTATUS codes. For the underlying status code, use [`status()`](Self::status).
    pub fn kind(&self) -> ErrorKind {
        match self {
            Error::NamedStreamsUnsupported { .. } | Error::StreamDeleteIgnored { .. } => {
                ErrorKind::Unsupported
            }
            Error::InvalidData { .. } => ErrorKind::InvalidData,
            Error::Auth { .. } => ErrorKind::AuthRequired,
            Error::SignatureInvalid { .. } => ErrorKind::SignatureInvalid,
            Error::GuestSessionRefused { .. } => ErrorKind::GuestSessionRefused,
            Error::Io(_) => ErrorKind::Io,
            Error::Disconnected => ErrorKind::ConnectionLost,
            Error::Timeout => ErrorKind::TimedOut,
            // Nothing answered on any address, so the link is what is missing.
            Error::ConnectFailed { .. } => ErrorKind::ConnectionLost,
            Error::Cancelled => ErrorKind::Cancelled,
            Error::SessionExpired => ErrorKind::SessionExpired,
            Error::DfsReferralRequired { .. } => ErrorKind::DfsReferral,
            // The namespace resolved; its storage is unreachable. That is a
            // connectivity answer, and the target may be back later.
            Error::DfsNoReachableTarget { .. } => ErrorKind::ConnectionLost,
            // The share's own connection is fine; only this link's far side
            // isn't, and reconnecting to the share wouldn't help.
            Error::DfsTargetUnreachable { .. } => ErrorKind::DfsTargetUnreachable,
            // Nothing happened to any connection: the policy said no.
            Error::DfsTargetNotAllowed { .. } => ErrorKind::DfsTargetNotAllowed,
            // A namespace that refers in circles is a server misconfiguration
            // with nothing behind it to find.
            Error::DfsTooManyReferrals { .. } => ErrorKind::NotFound,
            // The share exists, on a node this client won't follow to.
            Error::ShareRedirected { .. } => ErrorKind::Unsupported,
            Error::FileTooLargeForSingleRead { .. } => ErrorKind::TooLarge,
            // A connection whose credits never come back is a dead connection
            // wearing a live socket; consumers already reconnect on TimedOut.
            Error::CreditStarvation { .. } => ErrorKind::TimedOut,
            Error::SendTimeout { .. } => ErrorKind::TimedOut,
            // The socket failed under the write and the connection went with
            // it; what the OS called it doesn't change that.
            Error::SendFailed { .. } => ErrorKind::ConnectionLost,
            // The socket is up but nobody is home. Consumers already
            // reconnect on `ConnectionLost`, and reconnecting is the only
            // useful response, so it classifies with `Disconnected` rather
            // than with the per-request timeouts.
            Error::ServerUnresponsive { .. } => ErrorKind::ConnectionLost,
            // Deliberately NOT `cause`: the caller asked about the connection,
            // and the answer is that it is gone. The cause is there for a
            // consumer that wants to explain why.
            Error::ReconnectFailed { .. } => ErrorKind::ConnectionLost,
            Error::DurableHandleLost { .. } => ErrorKind::ConnectionLost,
            Error::ResumeRefused { .. } => ErrorKind::ResumeRefused,
            // The session the handle was opened on is the connection that was
            // lost; the revived one can't use it.
            Error::HandleInvalidated { .. } => ErrorKind::ConnectionLost,
            Error::Protocol { status, .. } => classify_status(*status),
        }
    }
}

/// Map an NTSTATUS to an ErrorKind.
fn classify_status(status: NtStatus) -> ErrorKind {
    match status {
        // Auth / signing -- the logon-rejection family. The credentials or the
        // account itself were refused (bad password, account disabled/expired/
        // locked, password expired, or a time/workstation restriction). All mean
        // the same thing to a consumer: this logon won't work, supply different
        // credentials. A guest/anonymous SessionSetup that the server rejects
        // (macOS smbd answers with STATUS_ACCOUNT_RESTRICTION) lands here too.
        NtStatus::LOGON_FAILURE
        | NtStatus::ACCOUNT_RESTRICTION
        | NtStatus::INVALID_LOGON_HOURS
        | NtStatus::INVALID_WORKSTATION
        | NtStatus::PASSWORD_EXPIRED
        | NtStatus::ACCOUNT_DISABLED
        | NtStatus::ACCOUNT_EXPIRED
        | NtStatus::PASSWORD_MUST_CHANGE
        | NtStatus::ACCOUNT_LOCKED_OUT => ErrorKind::AuthRequired,
        NtStatus::ACCESS_DENIED => {
            // Could be signing-required or genuinely access-denied.
            // Callers with NegotiatedParams context can distinguish further.
            // Default to AccessDenied; SmbClient methods can upgrade to
            // SigningRequired when signing_required is true.
            ErrorKind::AccessDenied
        }

        // The name can't be used at all, which is not the same as not finding
        // what it points at: the server never looked.
        NtStatus::OBJECT_NAME_INVALID => ErrorKind::InvalidName,

        // Not found
        NtStatus::NO_SUCH_FILE
        | NtStatus::OBJECT_NAME_NOT_FOUND
        | NtStatus::OBJECT_PATH_NOT_FOUND
        | NtStatus::BAD_NETWORK_NAME => ErrorKind::NotFound,

        // Already exists
        NtStatus::OBJECT_NAME_COLLISION => ErrorKind::AlreadyExists,

        // Wrong file type
        NtStatus::FILE_IS_A_DIRECTORY => ErrorKind::IsADirectory,
        NtStatus::NOT_A_DIRECTORY => ErrorKind::NotADirectory,

        // Sharing / locking
        NtStatus::SHARING_VIOLATION | NtStatus::FILE_LOCK_CONFLICT => ErrorKind::SharingViolation,

        // Disk full
        NtStatus::DISK_FULL => ErrorKind::DiskFull,

        // Session expired
        NtStatus::NETWORK_SESSION_EXPIRED => ErrorKind::SessionExpired,

        // Connection
        NtStatus::NETWORK_NAME_DELETED | NtStatus::USER_SESSION_DELETED => {
            ErrorKind::ConnectionLost
        }

        // DFS
        NtStatus::PATH_NOT_COVERED => ErrorKind::DfsReferral,

        // Unsupported operation (server lacks the feature, e.g. server-side copy)
        NtStatus::NOT_SUPPORTED | NtStatus::INVALID_DEVICE_REQUEST | NtStatus::NOT_IMPLEMENTED => {
            ErrorKind::Unsupported
        }

        // Everything else
        _ => ErrorKind::Other,
    }
}

/// A `Result` type alias using the crate's [`Error`](enum@Error) type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// Documents the full contract between `NtStatus` codes and `ErrorKind`.
    ///
    /// Every code listed here is asserted to map to its expected variant. When
    /// adding a new `NtStatus` to `types/status.rs`, also add a row here — either
    /// pointing at a dedicated `ErrorKind`, or `ErrorKind::Other` if there is
    /// genuinely no consumer-meaningful classification yet. The `classify_status_contract`
    /// test then asserts every row maps the way this table says it should, so the
    /// table stays in sync with what `classify_status` actually does.
    const STATUS_CLASSIFICATION_CONTRACT: &[(NtStatus, ErrorKind)] = &[
        // Auth / signing -- the logon-rejection family (credentials or the
        // account itself were refused; the caller needs different credentials)
        (NtStatus::LOGON_FAILURE, ErrorKind::AuthRequired),
        (NtStatus::ACCOUNT_RESTRICTION, ErrorKind::AuthRequired),
        (NtStatus::INVALID_LOGON_HOURS, ErrorKind::AuthRequired),
        (NtStatus::INVALID_WORKSTATION, ErrorKind::AuthRequired),
        (NtStatus::PASSWORD_EXPIRED, ErrorKind::AuthRequired),
        (NtStatus::ACCOUNT_DISABLED, ErrorKind::AuthRequired),
        (NtStatus::ACCOUNT_EXPIRED, ErrorKind::AuthRequired),
        (NtStatus::PASSWORD_MUST_CHANGE, ErrorKind::AuthRequired),
        (NtStatus::ACCOUNT_LOCKED_OUT, ErrorKind::AuthRequired),
        (NtStatus::ACCESS_DENIED, ErrorKind::AccessDenied),
        // Not found
        (NtStatus::NO_SUCH_FILE, ErrorKind::NotFound),
        (NtStatus::OBJECT_NAME_NOT_FOUND, ErrorKind::NotFound),
        (NtStatus::OBJECT_PATH_NOT_FOUND, ErrorKind::NotFound),
        (NtStatus::BAD_NETWORK_NAME, ErrorKind::NotFound),
        // Already exists
        (NtStatus::OBJECT_NAME_COLLISION, ErrorKind::AlreadyExists),
        // Unusable name
        (NtStatus::OBJECT_NAME_INVALID, ErrorKind::InvalidName),
        // Wrong file type
        (NtStatus::FILE_IS_A_DIRECTORY, ErrorKind::IsADirectory),
        (NtStatus::NOT_A_DIRECTORY, ErrorKind::NotADirectory),
        // Sharing / locking
        (NtStatus::SHARING_VIOLATION, ErrorKind::SharingViolation),
        (NtStatus::FILE_LOCK_CONFLICT, ErrorKind::SharingViolation),
        // Disk
        (NtStatus::DISK_FULL, ErrorKind::DiskFull),
        // Connection / session
        (NtStatus::NETWORK_NAME_DELETED, ErrorKind::ConnectionLost),
        (NtStatus::USER_SESSION_DELETED, ErrorKind::ConnectionLost),
        (NtStatus::NETWORK_SESSION_EXPIRED, ErrorKind::SessionExpired),
        // DFS
        (NtStatus::PATH_NOT_COVERED, ErrorKind::DfsReferral),
        // Unsupported operation
        (NtStatus::NOT_SUPPORTED, ErrorKind::Unsupported),
        (NtStatus::INVALID_DEVICE_REQUEST, ErrorKind::Unsupported),
        (NtStatus::NOT_IMPLEMENTED, ErrorKind::Unsupported),
        // Documented `Other` (no current consumer demand for a typed variant)
        (NtStatus::INVALID_PARAMETER, ErrorKind::Other),
        (NtStatus::DELETE_PENDING, ErrorKind::Other),
        (NtStatus::INSUFFICIENT_RESOURCES, ErrorKind::Other),
        (NtStatus::INSUFF_SERVER_RESOURCES, ErrorKind::Other),
    ];

    #[test]
    fn classify_status_contract() {
        for (status, expected) in STATUS_CLASSIFICATION_CONTRACT {
            let err = Error::Protocol {
                status: *status,
                command: Command::Create,
            };
            assert_eq!(
                err.kind(),
                *expected,
                "{status} should classify as {expected:?}"
            );
        }
    }

    #[test]
    fn kind_maps_non_protocol_errors() {
        assert_eq!(Error::Timeout.kind(), ErrorKind::TimedOut);
        assert_eq!(Error::Disconnected.kind(), ErrorKind::ConnectionLost);
        assert_eq!(Error::Cancelled.kind(), ErrorKind::Cancelled);
        assert_eq!(Error::SessionExpired.kind(), ErrorKind::SessionExpired);
        assert_eq!(Error::invalid_data("test").kind(), ErrorKind::InvalidData);
        assert_eq!(
            Error::FileTooLargeForSingleRead {
                size: 20_000_000,
                requested: 8_388_608,
            }
            .kind(),
            ErrorKind::TooLarge
        );
        assert!(
            !Error::FileTooLargeForSingleRead {
                size: 20_000_000,
                requested: 8_388_608,
            }
            .is_retryable(),
            "too-large is not fixed by retrying the same call"
        );
        assert_eq!(
            Error::DfsReferralRequired {
                path: "test".into()
            }
            .kind(),
            ErrorKind::DfsReferral
        );
        assert_eq!(
            Error::Auth {
                message: "test".into()
            }
            .kind(),
            ErrorKind::AuthRequired
        );
    }

    /// The security refusals have kinds of their own, so a consumer can tell
    /// "the connection is being tampered with" and "your password was taken
    /// as guest" from a malformed reply or a refused password, and neither
    /// is retried.
    #[test]
    fn security_refusals_classify_apart_and_are_not_retryable() {
        for reason in [SignatureFailure::Missing, SignatureFailure::Mismatch] {
            let err = Error::SignatureInvalid {
                command: Command::Read,
                message_id: crate::types::MessageId(42),
                reason,
            };
            assert_eq!(err.kind(), ErrorKind::SignatureInvalid);
            assert!(!err.is_retryable());
        }
        for anonymous in [false, true] {
            let err = Error::GuestSessionRefused {
                username: "alice".into(),
                anonymous,
            };
            assert_eq!(err.kind(), ErrorKind::GuestSessionRefused);
            assert!(!err.is_retryable());
        }
    }

    #[test]
    fn security_refusals_say_what_happened() {
        let missing = Error::SignatureInvalid {
            command: Command::Read,
            message_id: crate::types::MessageId(42),
            reason: SignatureFailure::Missing,
        };
        assert_eq!(
            missing.to_string(),
            "the server's Read response (message 42) isn't signed, so it was refused"
        );
        let mismatch = Error::SignatureInvalid {
            command: Command::SessionSetup,
            message_id: crate::types::MessageId(3),
            reason: SignatureFailure::Mismatch,
        };
        assert_eq!(
            mismatch.to_string(),
            "the server's SessionSetup response (message 3) has a signature that doesn't \
             match, so it was refused"
        );
        let guest = Error::GuestSessionRefused {
            username: "alice".into(),
            anonymous: false,
        };
        assert!(guest
            .to_string()
            .starts_with("the server offered a guest session instead of signing in as alice."));
        let anonymous = Error::GuestSessionRefused {
            username: "alice".into(),
            anonymous: true,
        };
        assert!(anonymous
            .to_string()
            .starts_with("the server offered an anonymous session instead"));
    }

    #[test]
    fn kind_maps_io_error_to_io_not_connection_lost() {
        // Error::Io from callback errors (like write_file_streamed cancellation)
        // should NOT be ConnectionLost — the connection may still be usable.
        let err = Error::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "cancelled",
        ));
        assert_eq!(err.kind(), ErrorKind::Io);
        assert_ne!(err.kind(), ErrorKind::ConnectionLost);
    }

    #[test]
    fn kind_disconnected_is_connection_lost() {
        // Error::Disconnected (transport EOF) IS a connection loss.
        assert_eq!(Error::Disconnected.kind(), ErrorKind::ConnectionLost);
    }

    #[test]
    fn kind_maps_dfs_referral_required_to_dfs_referral() {
        // The explicit DFS referral error variant should also map to DfsReferral.
        let err = Error::DfsReferralRequired {
            path: r"\\server\share\path".into(),
        };
        assert_eq!(err.kind(), ErrorKind::DfsReferral);
    }

    #[test]
    fn dfs_referral_is_not_retryable() {
        // DFS referrals need special handling, not generic retry.
        let err = Error::Protocol {
            status: NtStatus::PATH_NOT_COVERED,
            command: Command::Create,
        };
        assert!(!err.is_retryable());
    }
}
