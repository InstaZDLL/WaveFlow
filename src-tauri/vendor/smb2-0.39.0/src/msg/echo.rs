//! SMB2 ECHO request and response (spec sections 2.2.28, 2.2.29).
//!
//! Echo messages are used to check whether a server is processing requests.
//! Both request and response contain only a StructureSize field and a
//! reserved field, for a total of 4 bytes each.

super::trivial_message! {
    /// SMB2 ECHO request (spec section 2.2.28).
    ///
    /// Sent by the client to determine whether a server is processing requests.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct EchoRequest;
}

super::trivial_message! {
    /// SMB2 ECHO response (spec section 2.2.29).
    ///
    /// Sent by the server to confirm that an ECHO request was processed.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct EchoResponse;
}

#[cfg(test)]
mod tests {
    use super::*;

    super::super::trivial_message_tests!(
        EchoRequest,
        echo_request_known_bytes,
        echo_request_roundtrip,
        echo_request_wrong_structure_size,
        echo_request_too_short
    );

    super::super::trivial_message_tests!(
        EchoResponse,
        echo_response_known_bytes,
        echo_response_roundtrip,
        echo_response_wrong_structure_size,
        echo_response_too_short
    );
}
