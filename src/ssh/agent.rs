//! A blocking ssh-agent client.
//!
//! `sunset::agent` is the protocol; this is the socket under it. Unix only:
//! an agent on Windows is a named pipe rather than a Unix socket, so there
//! key files are the only option.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;

use sunset::{AuthSigMsg, Error, OwnedSig, Result, SignKey, agent};

/// A connection to a running ssh-agent.
pub struct AgentClient {
    conn: UnixStream,
    buf: Vec<u8>,
}

impl AgentClient {
    /// Connects to the agent listening on `path`, usually `$SSH_AUTH_SOCK`.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        Ok(Self {
            conn: UnixStream::connect(path)?,
            buf: Vec::new(),
        })
    }

    /// Sends one request and reads the reply, returning its body.
    fn round_trip(&mut self, req: &[u8]) -> Result<&[u8]> {
        self.conn.write_all(req)?;

        let mut frame = [0u8; 4];
        self.conn.read_exact(&mut frame)?;
        let len = agent::response_len(&frame)?;
        self.buf.clear();
        self.buf.resize(len, 0);
        self.conn.read_exact(&mut self.buf)?;
        Ok(&self.buf)
    }

    /// The keys the agent is holding, as keys that sign through it.
    pub fn keys(&mut self) -> Result<Vec<SignKey>> {
        let mut req = [0u8; 64];
        let n = agent::encode_request_identities(&mut req)?;
        let body = self.round_trip(&req[..n])?;

        match agent::parse_response(body)? {
            agent::AgentResponse::Identities(ids) => {
                let mut keys = Vec::with_capacity(ids.remaining() as usize);
                for id in ids {
                    let id = id?;
                    match id.sign_key() {
                        Ok(k) => keys.push(k),
                        // A key type sunset can't use is not a failure; the
                        // agent may hold others that work.
                        Err(e) => log::debug!("skipping agent key {:?}: {e}", id.comment),
                    }
                }
                Ok(keys)
            }
            _ => Err(Error::msg("unexpected agent response")),
        }
    }

    /// Has the agent sign an authentication request with one of its keys.
    pub fn sign_auth(&mut self, key: &SignKey, msg: &AuthSigMsg<'_>) -> Result<OwnedSig> {
        let mut req = vec![0u8; agent::sign_request_len(key, msg)?];
        let n = agent::encode_sign_request(&mut req, key, msg)?;
        let body = self.round_trip(&req[..n])?;

        match agent::parse_response(body)? {
            agent::AgentResponse::Signature(sig) => sig.try_into(),
            _ => Err(Error::msg("unexpected agent response")),
        }
    }
}
