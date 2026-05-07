include!(concat!(env!("OUT_DIR"), "/chatproj.rs"));

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn test_login_request_roundtrip() {
        let req = LoginRequest {
            username: "alice".into(),
            password: "secret123".into(),
        };
        let bytes = req.encode_to_vec();
        let decoded = LoginRequest::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.password, "secret123");
    }

    #[test]
    fn test_packet_with_oneof_payload() {
        let packet = Packet {
            r#type: packet::Type::LoginReq as i32,
            timestamp: 1234567890,
            auth_token: String::new(),
            payload: Some(packet::Payload::LoginReq(LoginRequest {
                username: "bob".into(),
                password: "pass".into(),
            })),
        };
        let bytes = packet.encode_to_vec();
        let decoded = Packet::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.r#type, packet::Type::LoginReq as i32);
        assert_eq!(decoded.timestamp, 1234567890);
        match decoded.payload {
            Some(packet::Payload::LoginReq(req)) => {
                assert_eq!(req.username, "bob");
                assert_eq!(req.password, "pass");
            }
            other => panic!("Expected LoginReq payload, got {:?}", other),
        }
    }

    #[test]
    fn test_login_response_with_jwt() {
        let resp = LoginResponse {
            success: true,
            message: "Welcome".into(),
            jwt_token: "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test".into(),
        };
        let bytes = resp.encode_to_vec();
        let decoded = LoginResponse::decode(&bytes[..]).unwrap();
        assert!(decoded.success);
        assert_eq!(decoded.jwt_token, "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test");
    }

    #[test]
    fn test_packet_auth_token_field() {
        let packet = Packet {
            r#type: packet::Type::ServerListReq as i32,
            timestamp: 0,
            auth_token: "my_jwt_token".into(),
            payload: Some(packet::Payload::ServerListReq(ServerListRequest {})),
        };
        let bytes = packet.encode_to_vec();
        let decoded = Packet::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.auth_token, "my_jwt_token");
    }
}
