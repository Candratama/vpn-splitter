use crate::{
    Error,
    routing::{NetworkInterface, RoutePlan},
    session::{ProfileId, SessionId},
};
use serde::{Deserialize, Serialize};

pub const MAX_MESSAGE_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub version: u8,
    pub session: SessionId,
    pub sequence: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Begin {
        header: Header,
        profile_id: ProfileId,
        vpn_interface: NetworkInterface,
    },
    Apply {
        header: Header,
        plan: RoutePlan,
    },
    Clear {
        header: Header,
    },
    Status {
        header: Header,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultCode {
    InvalidInput,
    Permission,
    Timeout,
    Conflict,
    UnsupportedProfile,
    Dns,
    Transport,
    Cleanup,
    Io,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplyResult {
    Begun,
    Applied,
    Cleared,
    Status {
        active: bool,
        cleanup_required: bool,
    },
    Rejected {
        code: FaultCode,
        previous_plan_restored: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub header: Header,
    pub result: ReplyResult,
}

impl Request {
    pub fn header(&self) -> Header {
        match self {
            Self::Begin { header, .. }
            | Self::Apply { header, .. }
            | Self::Clear { header }
            | Self::Status { header } => *header,
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let request: Self = decode(bytes)?;
        request.validate()?;
        Ok(request)
    }
    pub fn validate(&self) -> Result<(), Error> {
        self.header().validate()?;
        match self {
            Self::Begin { vpn_interface, .. } => vpn_interface.validate(),
            Self::Apply { plan, .. } => plan.validate(),
            _ => Ok(()),
        }
    }
}

impl Header {
    pub fn validate(&self) -> Result<(), Error> {
        if self.version != 1 || self.session.is_nil() || self.sequence == 0 {
            return Err(Error::Transport);
        }
        Ok(())
    }
}
fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Transport)?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Transport);
    }
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::Transport);
    }
    serde_json::from_slice(bytes).map_err(|_| Error::Transport)
}
impl Reply {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.header.validate()?;
        encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        let reply: Self = decode(bytes)?;
        reply.header.validate()?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Request {
        Request::Begin {
            header: Header {
                version: 1,
                session: SessionId::new(),
                sequence: 1,
            },
            profile_id: ProfileId::try_from("Example VPN".to_string()).unwrap(),
            vpn_interface: crate::routing::tests::snapshot().vpn,
        }
    }
    #[test]
    fn rejects_bad_headers_unknown_fields_and_preserves_typed_replies() {
        let r = request();
        let mut json = serde_json::to_value(&r).unwrap();
        json["extra"] = serde_json::json!(true);
        assert!(Request::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        for value in [
            serde_json::json!(0),
            serde_json::json!("00000000-0000-0000-0000-000000000000"),
        ] {
            let mut json = serde_json::to_value(&r).unwrap();
            let field = if value.is_number() {
                "sequence"
            } else {
                "session"
            };
            json["header"][field] = value;
            assert!(Request::decode(&serde_json::to_vec(&json).unwrap()).is_err());
        }
        let reply = Reply {
            header: r.header(),
            result: ReplyResult::Rejected {
                code: FaultCode::Conflict,
                previous_plan_restored: true,
            },
        };
        assert_eq!(Reply::decode(&reply.encode().unwrap()).unwrap(), reply);
        assert!(Reply::decode(b"{}").is_err());
    }

    #[test]
    fn accepts_typed_roundtrip_and_rejects_unknown_version_fields_and_truncation() {
        let request = request();
        let bytes = request.encode().unwrap();
        assert_eq!(Request::decode(&bytes).unwrap(), request);
        assert!(Request::decode(&bytes[..bytes.len() - 1]).is_err());
        let json = String::from_utf8(bytes)
            .unwrap()
            .replace("\"version\":1", "\"version\":2");
        assert!(Request::decode(json.as_bytes()).is_err());
        assert!(Request::decode(br#"{"operation":"execute","command":"example"}"#).is_err());
    }
    #[test]
    fn rejects_oversized_frames_and_invalid_plan_payloads() {
        assert!(Request::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]).is_err());
        let mut p = crate::routing::tests::plan();
        p.routes[0].prefix = 0;
        let r = Request::Apply {
            header: request().header(),
            plan: p,
        };
        assert!(r.encode().is_err());
    }
}
