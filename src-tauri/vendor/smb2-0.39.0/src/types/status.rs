//! NTSTATUS codes used by SMB2/3 (from MS-ERREF).

use std::fmt;

/// Defines `NtStatus` associated constants and the `name()` match arms from
/// a single table, so adding a new status code only requires one edit.
macro_rules! nt_status_codes {
    (
        $(
            $(#[$meta:meta])*
            $name:ident = $value:expr, $display:expr;
        )*
    ) => {
        impl NtStatus {
            $(
                $(#[$meta])*
                pub const $name: Self = Self($value);
            )*

            /// Returns a human-readable name for known status codes,
            /// or `None` for unknown codes.
            fn name(&self) -> Option<&'static str> {
                match self.0 {
                    $( $value => Some($display), )*
                    _ => None,
                }
            }
        }
    };
}

/// NT status code returned in SMB2 response headers.
///
/// The top two bits encode severity:
/// - `00` = success
/// - `01` = informational
/// - `10` = warning
/// - `11` = error
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NtStatus(pub u32);

nt_status_codes! {
    // -- Success (severity 0b00) --

    /// The operation completed successfully.
    SUCCESS = 0x0000_0000, "STATUS_SUCCESS";

    /// The operation that was requested is pending completion.
    PENDING = 0x0000_0103, "STATUS_PENDING";

    /// Oplock break notification (informational).
    NOTIFY_ENUM_DIR = 0x0000_010C, "STATUS_NOTIFY_ENUM_DIR";

    // -- Informational (severity 0b00, facility-specific) --

    /// The authentication exchange is not complete -- send the next
    /// SESSION_SETUP with the GSS token from this response.
    ///
    /// **Important:** The severity bits are 0b11 (error), so `is_error()`
    /// returns `true`. But this is NOT a real error -- it's a "keep going"
    /// signal during NTLM/SPNEGO auth. Auth code must check
    /// `is_more_processing_required()` before checking `is_error()`.
    MORE_PROCESSING_REQUIRED = 0xC000_0016, "STATUS_MORE_PROCESSING_REQUIRED";

    // -- Warnings (severity 0b10) --

    /// The data was too large to fit into the specified buffer.
    /// This is a warning -- the response body contains valid partial data.
    BUFFER_OVERFLOW = 0x8000_0005, "STATUS_BUFFER_OVERFLOW";

    /// No more files were found which match the file specification.
    NO_MORE_FILES = 0x8000_0006, "STATUS_NO_MORE_FILES";

    /// An open reached a symbolic link, which the server doesn't follow
    /// (MS-SMB2 § 2.2.2.2.1, § 3.3.5.9). The error data carries the link's
    /// target; opening with `FILE_OPEN_REPARSE_POINT` opens the link itself.
    STOPPED_ON_SYMLINK = 0x8000_002D, "STATUS_STOPPED_ON_SYMLINK";

    // -- Errors (severity 0b11) --

    /// The requested operation was unsuccessful.
    UNSUCCESSFUL = 0xC000_0001, "STATUS_UNSUCCESSFUL";

    /// The requested operation is not implemented.
    NOT_IMPLEMENTED = 0xC000_0002, "STATUS_NOT_IMPLEMENTED";

    /// The information class isn't a valid one for the request. A server
    /// answers a QUERY_DIRECTORY or QUERY_INFO class it doesn't know this way.
    INVALID_INFO_CLASS = 0xC000_0003, "STATUS_INVALID_INFO_CLASS";

    /// An invalid parameter was passed to a service or function.
    INVALID_PARAMETER = 0xC000_000D, "STATUS_INVALID_PARAMETER";

    /// A device that does not exist was specified.
    NO_SUCH_DEVICE = 0xC000_000E, "STATUS_NO_SUCH_DEVICE";

    /// The file does not exist.
    NO_SUCH_FILE = 0xC000_000F, "STATUS_NO_SUCH_FILE";

    /// The specified request is not a valid operation for the target device.
    INVALID_DEVICE_REQUEST = 0xC000_0010, "STATUS_INVALID_DEVICE_REQUEST";

    /// The end-of-file marker has been reached.
    END_OF_FILE = 0xC000_0011, "STATUS_END_OF_FILE";

    /// A process has requested access to an object but has not been
    /// granted those access rights.
    ACCESS_DENIED = 0xC000_0022, "STATUS_ACCESS_DENIED";

    /// The buffer is too small to contain the entry.
    BUFFER_TOO_SMALL = 0xC000_0023, "STATUS_BUFFER_TOO_SMALL";

    /// The object name is not valid on this server.
    ///
    /// Not "no such file": the name itself is unusable, so retrying it
    /// verbatim can only fail again. The usual cause is a character SMB2
    /// forbids, which [`crate::name`] maps away before it reaches the wire;
    /// what is left is a name that is invalid for a reason the mapping does
    /// not cover, such as a reserved Windows device name.
    OBJECT_NAME_INVALID = 0xC000_0033, "STATUS_OBJECT_NAME_INVALID";

    /// The object name is not found.
    OBJECT_NAME_NOT_FOUND = 0xC000_0034, "STATUS_OBJECT_NAME_NOT_FOUND";

    /// The object name already exists.
    OBJECT_NAME_COLLISION = 0xC000_0035, "STATUS_OBJECT_NAME_COLLISION";

    /// The path does not exist.
    OBJECT_PATH_NOT_FOUND = 0xC000_003A, "STATUS_OBJECT_PATH_NOT_FOUND";

    /// A file cannot be opened because the share access flags
    /// are incompatible.
    SHARING_VIOLATION = 0xC000_0043, "STATUS_SHARING_VIOLATION";

    /// A requested read/write cannot be granted due to a conflicting
    /// file lock.
    FILE_LOCK_CONFLICT = 0xC000_0054, "STATUS_FILE_LOCK_CONFLICT";

    /// A non-close operation has been requested of a file object that
    /// has a delete pending.
    DELETE_PENDING = 0xC000_0056, "STATUS_DELETE_PENDING";

    /// The disk is full.
    DISK_FULL = 0xC000_007F, "STATUS_DISK_FULL";

    /// The attempted logon is invalid. The user name or authentication
    /// information was rejected.
    LOGON_FAILURE = 0xC000_006D, "STATUS_LOGON_FAILURE";

    /// The user name and authentication information are valid, but an account
    /// restriction (such as time-of-day rules) prevented a successful logon.
    ACCOUNT_RESTRICTION = 0xC000_006E, "STATUS_ACCOUNT_RESTRICTION";

    /// The account has time restrictions and cannot log on at this time.
    INVALID_LOGON_HOURS = 0xC000_006F, "STATUS_INVALID_LOGON_HOURS";

    /// The account is restricted so it cannot log on from this workstation.
    INVALID_WORKSTATION = 0xC000_0070, "STATUS_INVALID_WORKSTATION";

    /// The account password has expired.
    PASSWORD_EXPIRED = 0xC000_0071, "STATUS_PASSWORD_EXPIRED";

    /// The referenced account is currently disabled.
    ACCOUNT_DISABLED = 0xC000_0072, "STATUS_ACCOUNT_DISABLED";

    /// The account has expired.
    ACCOUNT_EXPIRED = 0xC000_0193, "STATUS_ACCOUNT_EXPIRED";

    /// The account password must be changed before the first logon.
    PASSWORD_MUST_CHANGE = 0xC000_0224, "STATUS_PASSWORD_MUST_CHANGE";

    /// The account was automatically locked out after too many invalid logon
    /// or password-change attempts.
    ACCOUNT_LOCKED_OUT = 0xC000_0234, "STATUS_ACCOUNT_LOCKED_OUT";

    /// Insufficient system resources exist to complete the API.
    INSUFFICIENT_RESOURCES = 0xC000_009A, "STATUS_INSUFFICIENT_RESOURCES";

    /// An internal error occurred. Windows also answers a compounded operation
    /// that needed asynchronous processing this way, unless it was last in the
    /// chain (MS-SMB2 § 3.3.5.2.7, product behavior note 266).
    INTERNAL_ERROR = 0xC000_00E5, "STATUS_INTERNAL_ERROR";

    /// The file that was specified as a target is a directory.
    FILE_IS_A_DIRECTORY = 0xC000_00BA, "STATUS_FILE_IS_A_DIRECTORY";

    /// The request is not supported. Returned, for example, by a server that
    /// does not implement a requested FSCTL such as server-side copy.
    NOT_SUPPORTED = 0xC000_00BB, "STATUS_NOT_SUPPORTED";

    /// A limit of the underlying file system, such as a stream too big for
    /// the xattr that backs it.
    FILE_SYSTEM_LIMITATION = 0xC000_0427, "STATUS_FILE_SYSTEM_LIMITATION";

    /// The network path cannot be located.
    BAD_NETWORK_PATH = 0xC000_00BE, "STATUS_BAD_NETWORK_PATH";

    /// The network name was deleted.
    NETWORK_NAME_DELETED = 0xC000_00C9, "STATUS_NETWORK_NAME_DELETED";

    /// The specified share name cannot be found on the remote server.
    BAD_NETWORK_NAME = 0xC000_00CC, "STATUS_BAD_NETWORK_NAME";

    /// No more connections can be made to this remote computer at this time.
    REQUEST_NOT_ACCEPTED = 0xC000_00D0, "STATUS_REQUEST_NOT_ACCEPTED";

    /// The directory still has entries in it, so it cannot be deleted.
    DIRECTORY_NOT_EMPTY = 0xC000_0101, "STATUS_DIRECTORY_NOT_EMPTY";

    /// A requested opened file is not a directory.
    NOT_A_DIRECTORY = 0xC000_0103, "STATUS_NOT_A_DIRECTORY";

    /// The I/O request was canceled.
    CANCELLED = 0xC000_0120, "STATUS_CANCELLED";

    /// The file can't be deleted: it's read-only. Clear
    /// [`FileAttributes::READONLY`](crate::FileAttributes::READONLY) first.
    CANNOT_DELETE = 0xC000_0121, "STATUS_CANNOT_DELETE";

    /// An I/O request other than close was attempted using a file object
    /// that had already been closed.
    FILE_CLOSED = 0xC000_0128, "STATUS_FILE_CLOSED";

    /// The remote user session has been deleted.
    USER_SESSION_DELETED = 0xC000_0203, "STATUS_USER_SESSION_DELETED";

    /// Insufficient server resources exist to complete the request.
    INSUFF_SERVER_RESOURCES = 0xC000_0205, "STATUS_INSUFF_SERVER_RESOURCES";

    /// The object was not found.
    NOT_FOUND = 0xC000_0225, "STATUS_NOT_FOUND";

    /// The contacted server does not support the indicated part
    /// of the DFS namespace.
    PATH_NOT_COVERED = 0xC000_0257, "STATUS_PATH_NOT_COVERED";

    /// The client session has expired; the client must re-authenticate.
    NETWORK_SESSION_EXPIRED = 0xC000_035C, "STATUS_NETWORK_SESSION_EXPIRED";
}

impl NtStatus {
    // -- Helper methods --

    /// Returns the severity bits (top 2 bits): 0 = success, 1 = info,
    /// 2 = warning, 3 = error.
    #[inline]
    pub fn severity(&self) -> u8 {
        (self.0 >> 30) as u8
    }

    /// Returns `true` if the status indicates success (severity 0b00).
    #[inline]
    pub fn is_success(&self) -> bool {
        self.severity() == 0
    }

    /// Returns `true` if the status is a warning (severity 0b10).
    #[inline]
    pub fn is_warning(&self) -> bool {
        self.severity() == 2
    }

    /// Returns `true` if the status is an error (severity 0b11).
    #[inline]
    pub fn is_error(&self) -> bool {
        self.severity() == 3
    }

    /// Returns `true` if this is `STATUS_PENDING`.
    #[inline]
    pub fn is_pending(&self) -> bool {
        *self == Self::PENDING
    }

    /// Returns `true` if this status indicates the operation produced usable data.
    ///
    /// This includes `SUCCESS` and warnings like `BUFFER_OVERFLOW` where partial
    /// data is valid and should be parsed.
    #[inline]
    pub fn is_success_or_partial(&self) -> bool {
        self.is_success() || *self == Self::BUFFER_OVERFLOW
    }

    /// Returns `true` if the server wants another SESSION_SETUP round-trip.
    ///
    /// Check this BEFORE `is_error()` during authentication -- it has
    /// error severity bits but is not a real error.
    #[inline]
    pub fn is_more_processing_required(&self) -> bool {
        *self == Self::MORE_PROCESSING_REQUIRED
    }
}

impl fmt::Debug for NtStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => write!(f, "NtStatus({name})"),
            None => write!(f, "NtStatus(0x{:08X})", self.0),
        }
    }
}

impl fmt::Display for NtStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "0x{:08X}", self.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_is_success() {
        assert!(NtStatus::SUCCESS.is_success());
        assert!(!NtStatus::SUCCESS.is_error());
        assert!(!NtStatus::SUCCESS.is_warning());
        assert_eq!(NtStatus::SUCCESS.severity(), 0);
    }

    #[test]
    fn access_denied_is_error() {
        assert!(NtStatus::ACCESS_DENIED.is_error());
        assert!(!NtStatus::ACCESS_DENIED.is_success());
        assert!(!NtStatus::ACCESS_DENIED.is_warning());
        assert_eq!(NtStatus::ACCESS_DENIED.severity(), 3);
    }

    #[test]
    fn buffer_overflow_is_warning() {
        assert!(NtStatus::BUFFER_OVERFLOW.is_warning());
        assert!(!NtStatus::BUFFER_OVERFLOW.is_success());
        assert!(!NtStatus::BUFFER_OVERFLOW.is_error());
        assert_eq!(NtStatus::BUFFER_OVERFLOW.severity(), 2);
    }

    #[test]
    fn pending_is_pending() {
        assert!(NtStatus::PENDING.is_pending());
        assert!(NtStatus::PENDING.is_success()); // severity 0b00
        assert!(!NtStatus::SUCCESS.is_pending());
    }

    #[test]
    fn more_processing_required_is_error_severity() {
        // 0xC0000016 has severity 0b11 (error), even though semantically
        // it means "keep going" during authentication handshakes.
        assert!(NtStatus::MORE_PROCESSING_REQUIRED.is_error());
        assert_eq!(NtStatus::MORE_PROCESSING_REQUIRED.severity(), 3);
    }

    #[test]
    fn display_known_code() {
        assert_eq!(NtStatus::ACCESS_DENIED.to_string(), "STATUS_ACCESS_DENIED");
    }

    #[test]
    fn display_unknown_code() {
        let unknown = NtStatus(0xDEAD_BEEF);
        assert_eq!(unknown.to_string(), "0xDEADBEEF");
    }

    #[test]
    fn debug_known_code() {
        let s = format!("{:?}", NtStatus::SUCCESS);
        assert_eq!(s, "NtStatus(STATUS_SUCCESS)");
    }

    #[test]
    fn debug_unknown_code() {
        let s = format!("{:?}", NtStatus(0x1234_5678));
        assert_eq!(s, "NtStatus(0x12345678)");
    }

    #[test]
    fn no_more_files_is_warning() {
        assert!(NtStatus::NO_MORE_FILES.is_warning());
    }

    #[test]
    fn default_is_success() {
        assert_eq!(NtStatus::default(), NtStatus::SUCCESS);
    }

    #[test]
    fn is_success_or_partial() {
        // SUCCESS is usable
        assert!(NtStatus::SUCCESS.is_success_or_partial());
        // BUFFER_OVERFLOW is a warning with valid partial data
        assert!(NtStatus::BUFFER_OVERFLOW.is_success_or_partial());
        // Errors are not usable
        assert!(!NtStatus::ACCESS_DENIED.is_success_or_partial());
        // PENDING has success severity (0b00) so is_success() is true,
        // but callers handle PENDING separately before reaching status checks.
        assert!(NtStatus::PENDING.is_success_or_partial());
        // Other warnings (not BUFFER_OVERFLOW) are not usable
        assert!(!NtStatus::NO_MORE_FILES.is_success_or_partial());
    }

    #[test]
    fn the_status_for_an_unusable_name_renders_by_name() {
        // What a QNAP Samba share answered a CREATE carrying a raw `?`
        // (2026-08-05). Rendering it as raw hex told nobody anything.
        assert_eq!(NtStatus(0xC000_0033), NtStatus::OBJECT_NAME_INVALID);
        assert_eq!(
            NtStatus::OBJECT_NAME_INVALID.to_string(),
            "STATUS_OBJECT_NAME_INVALID"
        );
    }

    #[test]
    fn all_error_codes_have_error_severity() {
        let errors = [
            NtStatus::UNSUCCESSFUL,
            NtStatus::NOT_IMPLEMENTED,
            NtStatus::INVALID_INFO_CLASS,
            NtStatus::INVALID_PARAMETER,
            NtStatus::NO_SUCH_DEVICE,
            NtStatus::NO_SUCH_FILE,
            NtStatus::END_OF_FILE,
            NtStatus::ACCESS_DENIED,
            NtStatus::BUFFER_TOO_SMALL,
            NtStatus::OBJECT_NAME_INVALID,
            NtStatus::OBJECT_NAME_NOT_FOUND,
            NtStatus::OBJECT_NAME_COLLISION,
            NtStatus::OBJECT_PATH_NOT_FOUND,
            NtStatus::SHARING_VIOLATION,
            NtStatus::FILE_LOCK_CONFLICT,
            NtStatus::DELETE_PENDING,
            NtStatus::LOGON_FAILURE,
            NtStatus::ACCOUNT_RESTRICTION,
            NtStatus::INVALID_LOGON_HOURS,
            NtStatus::INVALID_WORKSTATION,
            NtStatus::PASSWORD_EXPIRED,
            NtStatus::ACCOUNT_DISABLED,
            NtStatus::ACCOUNT_EXPIRED,
            NtStatus::PASSWORD_MUST_CHANGE,
            NtStatus::ACCOUNT_LOCKED_OUT,
            NtStatus::INSUFFICIENT_RESOURCES,
            NtStatus::INTERNAL_ERROR,
            NtStatus::FILE_IS_A_DIRECTORY,
            NtStatus::NOT_SUPPORTED,
            NtStatus::BAD_NETWORK_PATH,
            NtStatus::NETWORK_NAME_DELETED,
            NtStatus::BAD_NETWORK_NAME,
            NtStatus::REQUEST_NOT_ACCEPTED,
            NtStatus::DIRECTORY_NOT_EMPTY,
            NtStatus::NOT_A_DIRECTORY,
            NtStatus::CANCELLED,
            NtStatus::CANNOT_DELETE,
            NtStatus::FILE_CLOSED,
            NtStatus::USER_SESSION_DELETED,
            NtStatus::INSUFF_SERVER_RESOURCES,
            NtStatus::NOT_FOUND,
            NtStatus::PATH_NOT_COVERED,
            NtStatus::NETWORK_SESSION_EXPIRED,
            NtStatus::MORE_PROCESSING_REQUIRED,
        ];
        for status in &errors {
            assert!(
                status.is_error(),
                "{status} should be error but severity is {}",
                status.severity()
            );
        }
    }
}
