//! A blocking ssh-agent client.
//!
//! Ported from `sunset-stdasync`, which is not usable here: it is async and
//! Unix-only, and this transport has no executor. The protocol itself is two
//! request types over a socket, so the port is the IO and nothing else.
//!
//! Unix only. Windows agents are named pipes rather than a Unix socket, so
//! there key files are the only option.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use sunset::sshnames::*;
use sunset::sshwire::{
    self, Blob, SSHDecode, SSHEncode, SSHSink, SSHSource, TextString, WireError, WireResult,
};
use sunset::{AuthSigMsg, Error, OwnedSig, PubKey, Result, SignKey, Signature};
use sunset_sshwire_derive::*;

/// Must be enough for the list of every public key the agent holds.
const MAX_RESPONSE: usize = 200_000;

#[derive(Debug, SSHEncode)]
struct AgentSignRequest<'a> {
    pub key_blob: Blob<PubKey<'a>>,
    pub msg: Blob<&'a AuthSigMsg<'a>>,
    pub flags: u32,
}

#[derive(Debug, SSHDecode)]
struct AgentSignResponse<'a> {
    pub sig: Blob<Signature<'a>>,
}

#[derive(Debug)]
struct AgentIdentitiesAnswer<'a> {
    /// `[(key blob, comment)]`
    pub keys: Vec<(PubKey<'a>, TextString<'a>)>,
}

#[derive(Debug)]
enum AgentRequest<'a> {
    SignRequest(AgentSignRequest<'a>),
    RequestIdentities,
}

impl SSHEncode for AgentRequest<'_> {
    fn enc(&self, s: &mut dyn SSHSink) -> WireResult<()> {
        match *self {
            Self::SignRequest(ref a) => {
                (AgentMessageNum::SSH_AGENTC_SIGN_REQUEST as u8).enc(s)?;
                a.enc(s)?;
            }
            Self::RequestIdentities => {
                (AgentMessageNum::SSH_AGENTC_REQUEST_IDENTITIES as u8).enc(s)?;
            }
        }
        Ok(())
    }
}

/// The subset of responses we recognise.
#[derive(Debug)]
enum AgentResponse<'a> {
    IdentitiesAnswer(AgentIdentitiesAnswer<'a>),
    SignResponse(AgentSignResponse<'a>),
}

impl<'de: 'a, 'a> SSHDecode<'de> for AgentResponse<'a> {
    fn dec<S>(s: &mut S) -> WireResult<Self>
    where
        S: SSHSource<'de>,
    {
        let number = u8::dec(s)?;
        if number == AgentMessageNum::SSH_AGENT_IDENTITIES_ANSWER as u8 {
            Ok(Self::IdentitiesAnswer(AgentIdentitiesAnswer::dec(s)?))
        } else if number == AgentMessageNum::SSH_AGENT_SIGN_RESPONSE as u8 {
            Ok(Self::SignResponse(AgentSignResponse::dec(s)?))
        } else {
            Err(WireError::UnknownPacket { number })
        }
    }
}

impl<'de: 'a, 'a> SSHDecode<'de> for AgentIdentitiesAnswer<'a> {
    fn dec<S>(s: &mut S) -> WireResult<Self>
    where
        S: SSHSource<'de>,
    {
        // uint32 nkeys, then that many (string key blob, string comment).
        let l = u32::dec(s)?;
        let mut keys = vec![];
        for _ in 0..l {
            let kb = Blob::<PubKey>::dec(s)?;
            let comment = TextString::dec(s)?;
            keys.push((kb.0, comment))
        }
        Ok(AgentIdentitiesAnswer { keys })
    }
}

/// A connection to a running ssh-agent.
pub struct AgentClient {
    conn: UnixStream,
    buf: Vec<u8>,
}

impl AgentClient {
    /// Connects to the agent listening on `path`, usually `$SSH_AUTH_SOCK`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let conn = UnixStream::connect(path)?;
        Ok(Self {
            conn,
            buf: Vec::new(),
        })
    }

    fn request(&mut self, r: AgentRequest<'_>) -> Result<AgentResponse<'_>> {
        let mut b = Vec::new();
        sshwire::ssh_push_vec(&mut b, &Blob(r))?;
        self.conn.write_all(&b)?;

        let mut l = [0u8; 4];
        self.conn.read_exact(&mut l)?;
        let l = u32::from_be_bytes(l) as usize;
        if l > MAX_RESPONSE {
            return Err(Error::msg("agent response too large"));
        }
        self.buf.resize(l, 0);
        self.conn.read_exact(&mut self.buf)?;
        let (r, _len) = sshwire::read_ssh::<AgentResponse>(&self.buf, None)?;
        Ok(r)
    }

    /// The keys the agent is holding, as keys that sign through it.
    pub fn keys(&mut self) -> Result<Vec<SignKey>> {
        match self.request(AgentRequest::RequestIdentities)? {
            AgentResponse::IdentitiesAnswer(i) => {
                let mut keys = vec![];
                for &(ref pk, comment) in i.keys.iter() {
                    match SignKey::from_agent_pubkey(pk) {
                        Ok(k) => keys.push(k),
                        Err(e) => log::debug!("skipping agent key {comment:?}: {e}"),
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::msg("unexpected agent response")),
        }
    }

    /// Has the agent sign an authentication request with one of its keys.
    pub fn sign_auth(&mut self, key: &SignKey, msg: &AuthSigMsg<'_>) -> Result<OwnedSig> {
        let flags = match *key {
            SignKey::AgentRSA(_) => SSH_AGENT_FLAG_RSA_SHA2_256,
            _ => 0,
        };
        let r = AgentRequest::SignRequest(AgentSignRequest {
            key_blob: Blob(key.pubkey()),
            msg: Blob(msg),
            flags,
        });
        match self.request(r)? {
            AgentResponse::SignResponse(s) => s.sig.0.try_into(),
            _ => Err(Error::msg("unexpected agent response")),
        }
    }
}
