//! SMB2 LOGOFF request and response (spec sections 2.2.7, 2.2.8).
//!
//! Logoff messages request and confirm termination of a session.
//! Both request and response contain only a StructureSize field and a
//! reserved field, for a total of 4 bytes each.

super::trivial_message! {
    /// SMB2 LOGOFF request (spec section 2.2.7).
    ///
    /// Sent by the client to request termination of a particular session.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct LogoffRequest;
}

super::trivial_message! {
    /// SMB2 LOGOFF response (spec section 2.2.8).
    ///
    /// Sent by the server to confirm that a LOGOFF request was processed.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct LogoffResponse;
}

#[cfg(test)]
mod tests {
    use super::*;

    super::super::trivial_message_tests!(
        LogoffRequest,
        logoff_request_known_bytes,
        logoff_request_roundtrip,
        logoff_request_wrong_structure_size,
        logoff_request_too_short
    );

    super::super::trivial_message_tests!(
        LogoffResponse,
        logoff_response_known_bytes,
        logoff_response_roundtrip,
        logoff_response_wrong_structure_size,
        logoff_response_too_short
    );
}
