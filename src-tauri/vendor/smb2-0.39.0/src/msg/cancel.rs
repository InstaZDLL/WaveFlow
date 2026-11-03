//! SMB2 CANCEL request (spec section 2.2.30).
//!
//! The CANCEL request is fire-and-forget: the client sends it to cancel a
//! previously sent message, and there is no corresponding response message.
//! The MessageId of the request to cancel is set in the SMB2 header.

super::trivial_message! {
    /// SMB2 CANCEL request (spec section 2.2.30).
    ///
    /// Sent by the client to cancel a previously sent message on the same
    /// transport connection. There is no response for this command.
    /// Contains only StructureSize (2 bytes) and Reserved (2 bytes).
    pub struct CancelRequest;
}

#[cfg(test)]
mod tests {
    use super::*;

    super::super::trivial_message_tests!(
        CancelRequest,
        cancel_request_known_bytes,
        cancel_request_roundtrip,
        cancel_request_wrong_structure_size,
        cancel_request_too_short
    );
}
