use std::{ffi::OsString, fmt};

macro_rules! enum_newtype {
    ($n:ident { $($v:ident),* $(,)? }) => {
        $(
            impl From<$v> for $n {
                fn from(v: $v) -> Self {
                    $n::$v(v)
                }
            }
        )*
    };
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ServerMessage {
    Spawn(Spawn),
    Kill(Kill),
    Input(Input),
}

enum_newtype!(ServerMessage { Spawn, Kill, Input });

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum ClientMessage {
    ChildStarted(ChildStarted),
    ChildStopped(ChildStopped),
    Output(Output),
}

enum_newtype!(ClientMessage {
    Output,
    ChildStarted,
    ChildStopped
});

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Spawn {
    pub(crate) psid: ProcessSequenceId,
    pub(crate) command: OsString,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Kill {
    pub(crate) psid: ProcessSequenceId,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Input {
    pub(crate) psid: ProcessSequenceId,
    pub(crate) content: Vec<u8>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ChildStarted {
    pub(crate) psid: ProcessSequenceId,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ChildStopped {
    pub(crate) psid: ProcessSequenceId,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Output {
    pub(crate) psid: ProcessSequenceId,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

impl Output {
    pub fn stdout(psid: ProcessSequenceId, stdout: Vec<u8>) -> Self {
        Self {
            psid,
            stdout,
            stderr: Default::default(),
        }
    }

    pub fn stderr(psid: ProcessSequenceId, stderr: Vec<u8>) -> Self {
        Self {
            psid,
            stdout: Default::default(),
            stderr,
        }
    }
}

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ProcessSequenceId(u32);

impl ProcessSequenceId {
    pub fn new(id: u32) -> Self {
        Self(id)
    }
}

impl fmt::Display for ProcessSequenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PSID<{}>", self.0)
    }
}
