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
            device_id: "0123456789abcdef0123456789abcdef".into(),
            ..Default::default()
        };
        let bytes = req.encode_to_vec();
        let decoded = LoginRequest::decode(&bytes[..]).unwrap();
        assert_eq!(decoded.username, "alice");
        assert_eq!(decoded.password, "secret123");
        assert_eq!(decoded.device_id, "0123456789abcdef0123456789abcdef");
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
                ..Default::default()
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
            memberships: vec![],
            stun_servers: vec![],
            call_signaling: false,
            e2ee_keys: false,
            presence_statuses: true,
            status: PresenceStatus::PresenceInvisible as i32,
            status_text: "back at 5".into(),
            status_emoji: "🍕".into(),
            status_clear_in: 3600,
            uid: 7,
            username: "ivy".into(),
            e2ee_uid_binding: true,
            renames: true,
            resume_token: "resume.me".into(),
            dm_friends_only: true,
            device_limit: false,
            signed_out: false,
            devices: true,
        };
        let bytes = resp.encode_to_vec();
        let decoded = LoginResponse::decode(&bytes[..]).unwrap();
        assert!(decoded.success);
        assert!(decoded.dm_friends_only && !decoded.device_limit);
        assert_eq!(decoded.resume_token, "resume.me");
        assert_eq!(decoded.jwt_token, "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.test");
        assert!(decoded.presence_statuses);
        assert_eq!(decoded.status, PresenceStatus::PresenceInvisible as i32);
        assert_eq!(decoded.status_text, "back at 5");
        assert_eq!(decoded.status_emoji, "🍕");
        assert_eq!(decoded.status_clear_in, 3600);
        assert_eq!((decoded.uid, decoded.username.as_str()), (7, "ivy"));
        assert!(decoded.e2ee_uid_binding && decoded.renames);
    }

    #[test]
    fn test_presence_delta_roundtrip() {
        let packet = Packet {
            r#type: packet::Type::PresenceDelta as i32,
            timestamp: 0,
            auth_token: String::new(),
            payload: Some(packet::Payload::PresenceDelta(PresenceDelta {
                user: Some(UserPresence {
                    username: "ivy".into(),
                    avatar_version: "v1".into(),
                    status: PresenceStatus::PresenceDnd as i32,
                    status_text: "deep work".into(),
                    status_emoji: String::new(),
                    uid: 7,
                }),
                online: true,
            })),
        };
        let decoded = Packet::decode(&packet.encode_to_vec()[..]).unwrap();
        match decoded.payload {
            Some(packet::Payload::PresenceDelta(d)) => {
                assert!(d.online);
                let u = d.user.unwrap();
                assert_eq!(u.username, "ivy");
                assert_eq!(u.uid, 7);
                assert_eq!(crate::presence::status_name(u.status), "dnd");
                assert_eq!(u.status_text, "deep work");
            }
            other => panic!("Expected PresenceDelta payload, got {:?}", other),
        }
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
