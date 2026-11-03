//! SMB2 TREE_DISCONNECT request and response (spec sections 2.2.11, 2.2.12).
//!
//! Tree disconnect messages request and confirm disconnection from a share.
//! Both request and response contain only a StructureSize field and a
//! reserved field, for a total of 4 bytes each.

super::trivial_message! {
    /// SMB2 TREE_DISCONNECT request (spec section 2.2.11).
    ///
    /// Sent by the client to request that the tree connect specified in the
    /// TreeId within the SMB2 header be disconnected.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct TreeDisconnectRequest;
}

super::trivial_message! {
    /// SMB2 TREE_DISCONNECT response (spec section 2.2.12).
    ///
    /// Sent by the server to confirm that a TREE_DISCONNECT request was processed.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct TreeDisconnectResponse;
}

#[cfg(test)]
mod tests {
    use super::*;

    super::super::trivial_message_tests!(
        TreeDisconnectRequest,
        tree_disconnect_request_known_bytes,
        tree_disconnect_request_roundtrip,
        tree_disconnect_request_wrong_structure_size,
        tree_disconnect_request_too_short
    );

    super::super::trivial_message_tests!(
        TreeDisconnectResponse,
        tree_disconnect_response_known_bytes,
        tree_disconnect_response_roundtrip,
        tree_disconnect_response_wrong_structure_size,
        tree_disconnect_response_too_short
    );
}
